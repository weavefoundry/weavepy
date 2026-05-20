//! Recursive-descent parser for Python source.
//!
//! Consumes the token stream from [`weavepy_lexer::tokenize`] and
//! produces a [`Module`]. Expression precedence follows the Python
//! language reference; chained comparisons (`a < b < c`) are
//! collapsed into a single [`Compare`] node like CPython does.
//!
//! The parser is hand-written so we own diagnostics end-to-end. It
//! intentionally rejects features the slice doesn't support
//! (`class`, `try`, `with`, `match`, `async`, `await`, `yield`,
//! f-string interpolation) with a [`ParseError::NotImplemented`]
//! that names the relevant follow-up RFC.

use weavepy_lexer::{Keyword, Span, Token, TokenKind};

use crate::ast::{
    Alias, Arg, Arguments, BinOp, BoolOp, CmpOp, Comprehension, Constant, Expr, ExprKind,
    Keyword as KwArg, Module, Stmt, StmtKind, UnaryOp,
};
use crate::error::ParseError;

pub(crate) fn parse(source: &str, tokens: Vec<Token>) -> Result<Module, ParseError> {
    let mut p = Parser::new(source, tokens);
    let module = p.parse_module()?;
    Ok(module)
}

struct Parser<'src> {
    source: &'src str,
    tokens: Vec<Token>,
    pos: usize,
}

impl<'src> Parser<'src> {
    fn new(source: &'src str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
        }
    }

    fn lexeme(&self, span: Span) -> &'src str {
        &self.source[span.start.0 as usize..span.end.0 as usize]
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_token(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_at(&self, k: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + k).map(|t| &t.kind)
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn check(&self, k: &TokenKind) -> bool {
        self.peek() == k
    }

    fn eat(&mut self, k: &TokenKind) -> bool {
        if self.check(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: &TokenKind, what: &str) -> Result<Token, ParseError> {
        if self.check(k) {
            Ok(self.bump())
        } else {
            Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: format!("expected {what}, got {:?}", self.peek()),
            })
        }
    }

    fn at_keyword(&self, kw: Keyword) -> bool {
        matches!(self.peek(), TokenKind::Keyword(k) if *k == kw)
    }

    // Skip any trivia (NL, COMMENT) between meaningful tokens.
    fn skip_trivia(&mut self) {
        while matches!(self.peek(), TokenKind::Nl | TokenKind::Comment) {
            self.pos += 1;
        }
    }

    fn skip_trivia_and_newlines(&mut self) {
        while matches!(
            self.peek(),
            TokenKind::Nl | TokenKind::Comment | TokenKind::Newline
        ) {
            self.pos += 1;
        }
    }

    // ============================================================
    // Statements
    // ============================================================

    fn parse_module(&mut self) -> Result<Module, ParseError> {
        let mut body = Vec::new();
        self.skip_trivia_and_newlines();
        while !matches!(self.peek(), TokenKind::Endmarker) {
            let stmt = self.parse_statement()?;
            body.push(stmt);
            self.skip_trivia_and_newlines();
        }
        Ok(Module { body })
    }

    fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        self.skip_trivia();
        match self.peek() {
            TokenKind::Keyword(kw) => match kw {
                Keyword::Def => self.parse_function_def(),
                Keyword::If => self.parse_if(),
                Keyword::While => self.parse_while(),
                Keyword::For => self.parse_for(),
                Keyword::Return => self.parse_return(),
                Keyword::Pass => self.simple_keyword_stmt(StmtKind::Pass),
                Keyword::Break => self.simple_keyword_stmt(StmtKind::Break),
                Keyword::Continue => self.simple_keyword_stmt(StmtKind::Continue),
                Keyword::Import => self.parse_import(),
                Keyword::From => self.parse_import_from(),
                Keyword::Global => self.parse_global(),
                Keyword::Nonlocal => self.parse_nonlocal(),
                Keyword::Class | Keyword::Try | Keyword::With | Keyword::Raise => {
                    Err(ParseError::NotImplemented {
                        span: self.peek_token().span,
                        feature: kw.as_str(),
                        rfc: rfc_for(*kw),
                    })
                }
                Keyword::Async | Keyword::Await | Keyword::Yield => {
                    Err(ParseError::NotImplemented {
                        span: self.peek_token().span,
                        feature: kw.as_str(),
                        rfc: "RFC 0006",
                    })
                }
                _ => self.parse_simple_statement(),
            },
            _ => self.parse_simple_statement(),
        }
    }

    fn simple_keyword_stmt(&mut self, kind: StmtKind) -> Result<Stmt, ParseError> {
        let tok = self.bump();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind,
            span: tok.span,
        })
    }

    fn consume_stmt_end(&mut self) -> Result<(), ParseError> {
        match self.peek() {
            TokenKind::Newline | TokenKind::Semi | TokenKind::Endmarker => {
                if !matches!(self.peek(), TokenKind::Endmarker) {
                    self.bump();
                }
                Ok(())
            }
            other => Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: format!("expected end of statement, got {other:?}"),
            }),
        }
    }

    fn parse_simple_statement(&mut self) -> Result<Stmt, ParseError> {
        let start_span = self.peek_token().span;

        // Try to parse an expression first; assignment / aug-assignment
        // / ann-assignment are disambiguated by lookahead.
        let first = self.parse_expression(true)?;

        // Augmented assignment.
        if let Some(op) = self.try_aug_op() {
            let value = self.parse_expression_list(true)?;
            self.consume_stmt_end()?;
            let span = start_span.merge(value.span);
            return Ok(Stmt {
                kind: StmtKind::AugAssign {
                    target: first,
                    op,
                    value,
                },
                span,
            });
        }

        // Annotated assignment: `target: annotation = value` (or just `target: annotation`).
        if self.check(&TokenKind::Colon) {
            self.bump();
            let annotation = self.parse_expression(false)?;
            let value = if self.eat(&TokenKind::Equal) {
                Some(self.parse_expression_list(true)?)
            } else {
                None
            };
            self.consume_stmt_end()?;
            let end_span = value.as_ref().map_or(annotation.span, |v| v.span);
            return Ok(Stmt {
                kind: StmtKind::AnnAssign {
                    target: first,
                    annotation,
                    value,
                },
                span: start_span.merge(end_span),
            });
        }

        // Chained assignment: `a = b = c = value`.
        if self.check(&TokenKind::Equal) {
            let mut targets = vec![first];
            while self.eat(&TokenKind::Equal) {
                // Peek-parse the right-hand side as expression list;
                // re-classify if another `=` follows.
                let next = self.parse_expression_list(true)?;
                if self.check(&TokenKind::Equal) {
                    targets.push(next);
                } else {
                    self.consume_stmt_end()?;
                    let span = start_span.merge(next.span);
                    return Ok(Stmt {
                        kind: StmtKind::Assign {
                            targets,
                            value: next,
                        },
                        span,
                    });
                }
            }
            // Shouldn't reach here: we only exit the loop after the
            // final RHS has been consumed by the branch above.
            unreachable!("assignment loop fell through");
        }

        // Plain expression statement.
        self.consume_stmt_end()?;
        let span = first.span;
        Ok(Stmt {
            kind: StmtKind::Expr(first),
            span,
        })
    }

    fn try_aug_op(&mut self) -> Option<BinOp> {
        let op = match self.peek() {
            TokenKind::PlusEqual => BinOp::Add,
            TokenKind::MinusEqual => BinOp::Sub,
            TokenKind::StarEqual => BinOp::Mult,
            TokenKind::SlashEqual => BinOp::Div,
            TokenKind::DoubleSlashEqual => BinOp::FloorDiv,
            TokenKind::PercentEqual => BinOp::Mod,
            TokenKind::DoubleStarEqual => BinOp::Pow,
            TokenKind::AmperEqual => BinOp::BitAnd,
            TokenKind::VbarEqual => BinOp::BitOr,
            TokenKind::CaretEqual => BinOp::BitXor,
            TokenKind::LeftShiftEqual => BinOp::LShift,
            TokenKind::RightShiftEqual => BinOp::RShift,
            TokenKind::AtEqual => BinOp::MatMult,
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    fn parse_function_def(&mut self) -> Result<Stmt, ParseError> {
        let def_tok = self.bump(); // `def`
        let name_tok = self.expect(&TokenKind::Name, "function name")?;
        let name = self.lexeme(name_tok.span).to_owned();
        self.expect(&TokenKind::LPar, "`(`")?;
        let args = self.parse_function_arguments()?;
        self.expect(&TokenKind::RPar, "`)`")?;
        // Optional return-type annotation `-> ...`
        if self.eat(&TokenKind::RArrow) {
            let _ = self.parse_expression(false)?;
        }
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let span_end = body.last().map_or(def_tok.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::FunctionDef { name, args, body },
            span: def_tok.span.merge(span_end),
        })
    }

    fn parse_function_arguments(&mut self) -> Result<Arguments, ParseError> {
        self.parse_arguments_inner(true)
    }

    fn parse_lambda_arguments(&mut self) -> Result<Arguments, ParseError> {
        self.parse_arguments_inner(false)
    }

    fn parse_arguments_inner(&mut self, allow_annotation: bool) -> Result<Arguments, ParseError> {
        let mut args = Arguments::default();
        if self.check(&TokenKind::RPar) || self.check(&TokenKind::Colon) {
            return Ok(args);
        }
        // We walk the list, accumulating into the right bucket based on
        // markers (`/`, `*`, `**`). State machine:
        // 0 = positional (becomes posonlyargs if we see `/`)
        // 1 = post-`/` positional-or-keyword
        // 2 = keyword-only (after `*`)
        let mut phase = 0u8;
        let mut had_default = false;
        loop {
            if self.check(&TokenKind::RPar) || self.check(&TokenKind::Colon) {
                break;
            }
            // `*args` or bare `*` separator.
            if self.eat(&TokenKind::Star) {
                if matches!(self.peek(), TokenKind::Name) {
                    let n = self.bump();
                    args.vararg = Some(Arg {
                        name: self.lexeme(n.span).to_owned(),
                        annotation: self.try_arg_annotation(allow_annotation)?,
                        span: n.span,
                    });
                }
                phase = 2;
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            // `**kwargs`.
            if self.eat(&TokenKind::DoubleStar) {
                let n = self.expect(&TokenKind::Name, "kwarg name")?;
                args.kwarg = Some(Arg {
                    name: self.lexeme(n.span).to_owned(),
                    annotation: self.try_arg_annotation(allow_annotation)?,
                    span: n.span,
                });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            // `/` separator: everything we've collected becomes posonly.
            if self.eat(&TokenKind::Slash) {
                args.posonlyargs = std::mem::take(&mut args.args);
                phase = 1;
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }

            let n = self.expect(&TokenKind::Name, "parameter name")?;
            let name = self.lexeme(n.span).to_owned();
            let annotation = self.try_arg_annotation(allow_annotation)?;
            let default = if self.eat(&TokenKind::Equal) {
                Some(self.parse_expression(false)?)
            } else {
                None
            };
            let arg = Arg {
                name,
                annotation,
                span: n.span,
            };
            if phase == 2 {
                args.kwonlyargs.push(arg);
                args.kw_defaults.push(default);
            } else {
                args.args.push(arg);
                if let Some(d) = default {
                    args.defaults.push(d);
                    had_default = true;
                } else if had_default {
                    return Err(ParseError::Unexpected {
                        span: n.span,
                        message: "non-default argument follows default argument".to_owned(),
                    });
                }
            }

            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(args)
    }

    fn try_arg_annotation(&mut self, allow: bool) -> Result<Option<Box<Expr>>, ParseError> {
        if allow && self.eat(&TokenKind::Colon) {
            Ok(Some(Box::new(self.parse_expression(false)?)))
        } else {
            Ok(None)
        }
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let if_tok = self.bump(); // `if`
        let test = self.parse_expression(false)?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let orelse = if self.at_keyword(Keyword::Elif) {
            // Recurse: elif → nested `if`.
            let nested = self.parse_if()?;
            vec![nested]
        } else if self.at_keyword(Keyword::Else) {
            self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block()?
        } else {
            Vec::new()
        };
        let span_end = orelse
            .last()
            .or_else(|| body.last())
            .map_or(if_tok.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::If { test, body, orelse },
            span: if_tok.span.merge(span_end),
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let test = self.parse_expression(false)?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let orelse = if self.at_keyword(Keyword::Else) {
            self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block()?
        } else {
            Vec::new()
        };
        let span_end = orelse
            .last()
            .or_else(|| body.last())
            .map_or(kw.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::While { test, body, orelse },
            span: kw.span.merge(span_end),
        })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        // For-loop targets are a constrained subset of expressions —
        // notably, they must not be parsed as comparisons, otherwise
        // `for i in xs` is mis-read as `for (i in xs)`. Use the
        // sub-comparison level.
        let target = self.parse_target_list_no_tuple()?;
        if !self.at_keyword(Keyword::In) {
            return Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: "expected `in` in for-loop".to_owned(),
            });
        }
        self.bump();
        let iter = self.parse_expression_list(false)?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let orelse = if self.at_keyword(Keyword::Else) {
            self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block()?
        } else {
            Vec::new()
        };
        let span_end = orelse
            .last()
            .or_else(|| body.last())
            .map_or(kw.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::For {
                target,
                iter,
                body,
                orelse,
            },
            span: kw.span.merge(span_end),
        })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let value = if matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semi | TokenKind::Endmarker
        ) {
            None
        } else {
            Some(self.parse_expression_list(true)?)
        };
        self.consume_stmt_end()?;
        let end = value.as_ref().map_or(kw.span, |e| e.span);
        Ok(Stmt {
            kind: StmtKind::Return(value),
            span: kw.span.merge(end),
        })
    }

    fn parse_import(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let mut names = Vec::new();
        loop {
            let dotted = self.parse_dotted_name()?;
            let asname = if self.at_keyword(Keyword::As) {
                self.bump();
                let n = self.expect(&TokenKind::Name, "name after `as`")?;
                Some(self.lexeme(n.span).to_owned())
            } else {
                None
            };
            names.push(Alias {
                name: dotted,
                asname,
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Import(names),
            span: kw.span,
        })
    }

    fn parse_dotted_name(&mut self) -> Result<String, ParseError> {
        let first = self.expect(&TokenKind::Name, "module name")?;
        let mut out = self.lexeme(first.span).to_owned();
        while self.eat(&TokenKind::Dot) {
            let n = self.expect(&TokenKind::Name, "name after `.`")?;
            out.push('.');
            out.push_str(self.lexeme(n.span));
        }
        Ok(out)
    }

    fn parse_import_from(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump(); // `from`
        let mut level = 0u32;
        while self.eat(&TokenKind::Dot) {
            level += 1;
        }
        let module = if matches!(self.peek(), TokenKind::Name) {
            Some(self.parse_dotted_name()?)
        } else {
            None
        };
        if !self.at_keyword(Keyword::Import) {
            return Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: "expected `import`".to_owned(),
            });
        }
        self.bump();
        let names = if self.eat(&TokenKind::Star) {
            vec![Alias {
                name: "*".to_owned(),
                asname: None,
            }]
        } else {
            let paren = self.eat(&TokenKind::LPar);
            let mut names = Vec::new();
            loop {
                let n = self.expect(&TokenKind::Name, "imported name")?;
                let name = self.lexeme(n.span).to_owned();
                let asname = if self.at_keyword(Keyword::As) {
                    self.bump();
                    let n2 = self.expect(&TokenKind::Name, "name after `as`")?;
                    Some(self.lexeme(n2.span).to_owned())
                } else {
                    None
                };
                names.push(Alias { name, asname });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            if paren {
                self.expect(&TokenKind::RPar, "`)`")?;
            }
            names
        };
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::ImportFrom {
                module,
                names,
                level,
            },
            span: kw.span,
        })
    }

    fn parse_global(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let names = self.parse_name_list()?;
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Global(names),
            span: kw.span,
        })
    }

    fn parse_nonlocal(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let names = self.parse_name_list()?;
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Nonlocal(names),
            span: kw.span,
        })
    }

    fn parse_name_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut names = Vec::new();
        loop {
            let n = self.expect(&TokenKind::Name, "name")?;
            names.push(self.lexeme(n.span).to_owned());
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(names)
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, ParseError> {
        // After the `:`, either a single inline statement or a
        // NEWLINE INDENT block DEDENT.
        if matches!(self.peek(), TokenKind::Newline) {
            self.bump();
            self.skip_trivia_and_newlines();
            self.expect(&TokenKind::Indent, "indented block")?;
            let mut body = Vec::new();
            loop {
                self.skip_trivia_and_newlines();
                if matches!(self.peek(), TokenKind::Dedent | TokenKind::Endmarker) {
                    break;
                }
                let s = self.parse_statement()?;
                body.push(s);
            }
            self.eat(&TokenKind::Dedent);
            if body.is_empty() {
                return Err(ParseError::Unexpected {
                    span: self.peek_token().span,
                    message: "empty block".to_owned(),
                });
            }
            Ok(body)
        } else {
            // Inline single-statement block: `if x: y = 1`
            let s = self.parse_simple_statement()?;
            Ok(vec![s])
        }
    }

    // ============================================================
    // Expressions
    // ============================================================

    /// Parse one expression. If `allow_tuple` is true, top-level
    /// `,` builds a `Tuple`; if false, comma is a delimiter that
    /// ends the expression.
    fn parse_expression(&mut self, _allow_tuple: bool) -> Result<Expr, ParseError> {
        self.parse_ternary()
    }

    /// Parse a tuple-or-expression as it appears on the right side
    /// of `=` or `return`.
    fn parse_expression_list(&mut self, _allow_trailing_comma: bool) -> Result<Expr, ParseError> {
        let first = self.parse_ternary()?;
        if !self.check(&TokenKind::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        let start_span = items[0].span;
        while self.eat(&TokenKind::Comma) {
            if matches!(
                self.peek(),
                TokenKind::Newline
                    | TokenKind::Semi
                    | TokenKind::Endmarker
                    | TokenKind::RPar
                    | TokenKind::RSqb
                    | TokenKind::RBrace
                    | TokenKind::Colon
                    | TokenKind::Equal
            ) {
                break;
            }
            items.push(self.parse_ternary()?);
        }
        let end_span = items.last().expect("nonempty").span;
        Ok(Expr {
            kind: ExprKind::Tuple(items),
            span: start_span.merge(end_span),
        })
    }

    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        if self.at_keyword(Keyword::Lambda) {
            return self.parse_lambda();
        }
        let body = self.parse_or()?;
        if self.at_keyword(Keyword::If) {
            self.bump();
            let test = self.parse_or()?;
            if !self.at_keyword(Keyword::Else) {
                return Err(ParseError::Unexpected {
                    span: self.peek_token().span,
                    message: "expected `else` in conditional expression".to_owned(),
                });
            }
            self.bump();
            let orelse = self.parse_ternary()?;
            let span = body.span.merge(orelse.span);
            return Ok(Expr {
                kind: ExprKind::IfExp {
                    test: Box::new(test),
                    body: Box::new(body),
                    orelse: Box::new(orelse),
                },
                span,
            });
        }
        Ok(body)
    }

    fn parse_lambda(&mut self) -> Result<Expr, ParseError> {
        let kw = self.bump(); // `lambda`
        let args = if self.check(&TokenKind::Colon) {
            Arguments::default()
        } else {
            self.parse_lambda_arguments()?
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_ternary()?;
        let span = kw.span.merge(body.span);
        Ok(Expr {
            kind: ExprKind::Lambda {
                args,
                body: Box::new(body),
            },
            span,
        })
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_and()?;
        if !self.at_keyword(Keyword::Or) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.at_keyword(Keyword::Or) {
            self.bump();
            values.push(self.parse_and()?);
        }
        let span = values
            .first()
            .unwrap()
            .span
            .merge(values.last().unwrap().span);
        Ok(Expr {
            kind: ExprKind::BoolOp {
                op: BoolOp::Or,
                values,
            },
            span,
        })
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_not()?;
        if !self.at_keyword(Keyword::And) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.at_keyword(Keyword::And) {
            self.bump();
            values.push(self.parse_not()?);
        }
        let span = values
            .first()
            .unwrap()
            .span
            .merge(values.last().unwrap().span);
        Ok(Expr {
            kind: ExprKind::BoolOp {
                op: BoolOp::And,
                values,
            },
            span,
        })
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.at_keyword(Keyword::Not) {
            let kw = self.bump();
            let operand = self.parse_not()?;
            let span = kw.span.merge(operand.span);
            return Ok(Expr {
                kind: ExprKind::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_bit_or()?;
        let mut ops = Vec::new();
        let mut comparators = Vec::new();
        while let Some(op) = self.try_cmp_op() {
            ops.push(op);
            comparators.push(self.parse_bit_or()?);
        }
        if ops.is_empty() {
            return Ok(left);
        }
        let span = left.span.merge(comparators.last().unwrap().span);
        Ok(Expr {
            kind: ExprKind::Compare {
                left: Box::new(left),
                ops,
                comparators,
            },
            span,
        })
    }

    fn try_cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            TokenKind::Less => CmpOp::Lt,
            TokenKind::Greater => CmpOp::Gt,
            TokenKind::LessEqual => CmpOp::LtE,
            TokenKind::GreaterEqual => CmpOp::GtE,
            TokenKind::EqEqual => CmpOp::Eq,
            TokenKind::NotEqual => CmpOp::NotEq,
            TokenKind::Keyword(Keyword::In) => CmpOp::In,
            TokenKind::Keyword(Keyword::Is) => {
                // Two-token `is not` handled below.
                self.bump();
                if self.at_keyword(Keyword::Not) {
                    self.bump();
                    return Some(CmpOp::IsNot);
                }
                return Some(CmpOp::Is);
            }
            TokenKind::Keyword(Keyword::Not) => {
                // `not in`
                if matches!(self.peek_at(1), Some(TokenKind::Keyword(Keyword::In))) {
                    self.bump();
                    self.bump();
                    return Some(CmpOp::NotIn);
                }
                return None;
            }
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bit_xor()?;
        while self.check(&TokenKind::Vbar) {
            self.bump();
            let right = self.parse_bit_xor()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op: BinOp::BitOr,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_bit_and()?;
        while self.check(&TokenKind::Caret) {
            self.bump();
            let right = self.parse_bit_and()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op: BinOp::BitXor,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_bit_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_shift()?;
        while self.check(&TokenKind::Amper) {
            self.bump();
            let right = self.parse_shift()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op: BinOp::BitAnd,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_addsub()?;
        loop {
            let op = match self.peek() {
                TokenKind::LeftShift => BinOp::LShift,
                TokenKind::RightShift => BinOp::RShift,
                _ => break,
            };
            self.bump();
            let right = self.parse_addsub()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_addsub(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_muldiv()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let right = self.parse_muldiv()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_muldiv(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mult,
                TokenKind::Slash => BinOp::Div,
                TokenKind::DoubleSlash => BinOp::FloorDiv,
                TokenKind::Percent => BinOp::Mod,
                TokenKind::At => BinOp::MatMult,
                _ => break,
            };
            self.bump();
            let right = self.parse_unary()?;
            let span = left.span.merge(right.span);
            left = Expr {
                kind: ExprKind::BinOp {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                span,
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek() {
            TokenKind::Plus => UnaryOp::UAdd,
            TokenKind::Minus => UnaryOp::USub,
            TokenKind::Tilde => UnaryOp::Invert,
            _ => return self.parse_power(),
        };
        let kw = self.bump();
        let operand = self.parse_unary()?;
        let span = kw.span.merge(operand.span);
        Ok(Expr {
            kind: ExprKind::UnaryOp {
                op,
                operand: Box::new(operand),
            },
            span,
        })
    }

    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let base = self.parse_trailer_chain()?;
        if !self.check(&TokenKind::DoubleStar) {
            return Ok(base);
        }
        self.bump();
        // `**` is right-associative, and binds tighter than unary on
        // the right side. Python's grammar: `power: await? primary ('**' factor)?`
        // where `factor` includes unary.
        let exponent = self.parse_unary()?;
        let span = base.span.merge(exponent.span);
        Ok(Expr {
            kind: ExprKind::BinOp {
                left: Box::new(base),
                op: BinOp::Pow,
                right: Box::new(exponent),
            },
            span,
        })
    }

    fn parse_trailer_chain(&mut self) -> Result<Expr, ParseError> {
        let mut base = self.parse_atom()?;
        loop {
            match self.peek() {
                TokenKind::LPar => {
                    self.bump();
                    let (args, keywords) = self.parse_call_args()?;
                    let rp = self.expect(&TokenKind::RPar, "`)`")?;
                    let span = base.span.merge(rp.span);
                    base = Expr {
                        kind: ExprKind::Call {
                            func: Box::new(base),
                            args,
                            keywords,
                        },
                        span,
                    };
                }
                TokenKind::LSqb => {
                    self.bump();
                    let slice = self.parse_subscript()?;
                    let rb = self.expect(&TokenKind::RSqb, "`]`")?;
                    let span = base.span.merge(rb.span);
                    base = Expr {
                        kind: ExprKind::Subscript {
                            value: Box::new(base),
                            slice: Box::new(slice),
                        },
                        span,
                    };
                }
                TokenKind::Dot => {
                    self.bump();
                    let n = self.expect(&TokenKind::Name, "attribute name")?;
                    let attr = self.lexeme(n.span).to_owned();
                    let span = base.span.merge(n.span);
                    base = Expr {
                        kind: ExprKind::Attribute {
                            value: Box::new(base),
                            attr,
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(base)
    }

    fn parse_call_args(&mut self) -> Result<(Vec<Expr>, Vec<KwArg>), ParseError> {
        let mut args = Vec::new();
        let mut keywords = Vec::new();
        if self.check(&TokenKind::RPar) {
            return Ok((args, keywords));
        }
        loop {
            if self.eat(&TokenKind::DoubleStar) {
                let val = self.parse_ternary()?;
                keywords.push(KwArg {
                    arg: None,
                    value: val,
                });
            } else if self.eat(&TokenKind::Star) {
                let val = self.parse_ternary()?;
                let span = val.span;
                args.push(Expr {
                    kind: ExprKind::Starred(Box::new(val)),
                    span,
                });
            } else {
                // Could be a keyword arg `name=value` — peek ahead.
                if matches!(self.peek(), TokenKind::Name)
                    && matches!(self.peek_at(1), Some(TokenKind::Equal))
                {
                    let nt = self.bump();
                    let name = self.lexeme(nt.span).to_owned();
                    self.bump(); // `=`
                    let val = self.parse_ternary()?;
                    keywords.push(KwArg {
                        arg: Some(name),
                        value: val,
                    });
                } else {
                    let e = self.parse_ternary()?;
                    // Generator expression as single argument: `f(x for x in xs)`.
                    if self.at_keyword(Keyword::For) && args.is_empty() && keywords.is_empty() {
                        let elt = e;
                        let generators = self.parse_comp_for()?;
                        let span = elt.span;
                        args.push(Expr {
                            kind: ExprKind::GeneratorExp {
                                elt: Box::new(elt),
                                generators,
                            },
                            span,
                        });
                    } else {
                        args.push(e);
                    }
                }
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.check(&TokenKind::RPar) {
                break;
            }
        }
        Ok((args, keywords))
    }

    fn parse_subscript(&mut self) -> Result<Expr, ParseError> {
        // Slice grammar: `lower? ':' upper? (':' step?)?` or plain expr.
        if self.check(&TokenKind::Colon) {
            self.bump();
            let upper = if matches!(self.peek(), TokenKind::Colon | TokenKind::RSqb) {
                None
            } else {
                Some(Box::new(self.parse_ternary()?))
            };
            let step = if self.eat(&TokenKind::Colon) {
                if matches!(self.peek(), TokenKind::RSqb) {
                    None
                } else {
                    Some(Box::new(self.parse_ternary()?))
                }
            } else {
                None
            };
            let span = self.peek_token().span;
            return Ok(Expr {
                kind: ExprKind::Slice {
                    lower: None,
                    upper,
                    step,
                },
                span,
            });
        }
        let first = self.parse_ternary()?;
        if !self.check(&TokenKind::Colon) {
            return Ok(first);
        }
        // a:b:c
        self.bump();
        let upper = if matches!(self.peek(), TokenKind::Colon | TokenKind::RSqb) {
            None
        } else {
            Some(Box::new(self.parse_ternary()?))
        };
        let step = if self.eat(&TokenKind::Colon) {
            if matches!(self.peek(), TokenKind::RSqb) {
                None
            } else {
                Some(Box::new(self.parse_ternary()?))
            }
        } else {
            None
        };
        let span = first.span;
        Ok(Expr {
            kind: ExprKind::Slice {
                lower: Some(Box::new(first)),
                upper,
                step,
            },
            span,
        })
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek_token().clone();
        match &tok.kind {
            TokenKind::Number => {
                self.bump();
                let value =
                    parse_number(self.lexeme(tok.span)).map_err(|m| ParseError::Unexpected {
                        span: tok.span,
                        message: m,
                    })?;
                Ok(Expr {
                    kind: ExprKind::Constant(value),
                    span: tok.span,
                })
            }
            TokenKind::String => {
                // Adjacent string concatenation.
                let mut concatenated = self.decode_string(&tok)?;
                let mut last_span = tok.span;
                self.bump();
                while matches!(self.peek(), TokenKind::String) {
                    let next_tok = self.peek_token().clone();
                    let next = self.decode_string(&next_tok)?;
                    last_span = next_tok.span;
                    self.bump();
                    match (&mut concatenated, &next) {
                        (Constant::Str(a), Constant::Str(b)) => a.push_str(b),
                        (Constant::Bytes(a), Constant::Bytes(b)) => a.extend_from_slice(b),
                        _ => {
                            return Err(ParseError::Unexpected {
                                span: next_tok.span,
                                message: "cannot concatenate str and bytes literals".to_owned(),
                            });
                        }
                    }
                }
                Ok(Expr {
                    kind: ExprKind::Constant(concatenated),
                    span: tok.span.merge(last_span),
                })
            }
            TokenKind::Name => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Name(self.lexeme(tok.span).to_owned()),
                    span: tok.span,
                })
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Constant(Constant::Bool(true)),
                    span: tok.span,
                })
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Constant(Constant::Bool(false)),
                    span: tok.span,
                })
            }
            TokenKind::Keyword(Keyword::None) => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Constant(Constant::None),
                    span: tok.span,
                })
            }
            TokenKind::Ellipsis => {
                self.bump();
                Ok(Expr {
                    kind: ExprKind::Constant(Constant::Ellipsis),
                    span: tok.span,
                })
            }
            TokenKind::LPar => self.parse_paren_or_tuple(),
            TokenKind::LSqb => self.parse_list_or_listcomp(),
            TokenKind::LBrace => self.parse_dict_or_set(),
            other => Err(ParseError::Unexpected {
                span: tok.span,
                message: format!("unexpected token in expression: {other:?}"),
            }),
        }
    }

    fn parse_paren_or_tuple(&mut self) -> Result<Expr, ParseError> {
        let lp = self.bump();
        if self.eat(&TokenKind::RPar) {
            // Empty tuple
            return Ok(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: lp.span,
            });
        }
        let first = self.parse_ternary()?;
        // Generator expression?
        if self.at_keyword(Keyword::For) {
            let generators = self.parse_comp_for()?;
            let rp = self.expect(&TokenKind::RPar, "`)`")?;
            let span = lp.span.merge(rp.span);
            return Ok(Expr {
                kind: ExprKind::GeneratorExp {
                    elt: Box::new(first),
                    generators,
                },
                span,
            });
        }
        if self.check(&TokenKind::Comma) {
            let mut items = vec![first];
            while self.eat(&TokenKind::Comma) {
                if self.check(&TokenKind::RPar) {
                    break;
                }
                items.push(self.parse_ternary()?);
            }
            let rp = self.expect(&TokenKind::RPar, "`)`")?;
            return Ok(Expr {
                kind: ExprKind::Tuple(items),
                span: lp.span.merge(rp.span),
            });
        }
        // Plain parenthesized expression — keep its span but no wrapper node.
        let rp = self.expect(&TokenKind::RPar, "`)`")?;
        Ok(Expr {
            kind: first.kind,
            span: lp.span.merge(rp.span),
        })
    }

    fn parse_list_or_listcomp(&mut self) -> Result<Expr, ParseError> {
        let lb = self.bump();
        if self.eat(&TokenKind::RSqb) {
            return Ok(Expr {
                kind: ExprKind::List(Vec::new()),
                span: lb.span,
            });
        }
        let first = self.parse_ternary()?;
        if self.at_keyword(Keyword::For) {
            let generators = self.parse_comp_for()?;
            let rb = self.expect(&TokenKind::RSqb, "`]`")?;
            return Ok(Expr {
                kind: ExprKind::ListComp {
                    elt: Box::new(first),
                    generators,
                },
                span: lb.span.merge(rb.span),
            });
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.check(&TokenKind::RSqb) {
                break;
            }
            items.push(self.parse_ternary()?);
        }
        let rb = self.expect(&TokenKind::RSqb, "`]`")?;
        Ok(Expr {
            kind: ExprKind::List(items),
            span: lb.span.merge(rb.span),
        })
    }

    fn parse_dict_or_set(&mut self) -> Result<Expr, ParseError> {
        let lb = self.bump();
        if self.eat(&TokenKind::RBrace) {
            return Ok(Expr {
                kind: ExprKind::Dict {
                    keys: Vec::new(),
                    values: Vec::new(),
                },
                span: lb.span,
            });
        }
        // Look ahead to see if it's a dict (key:value) or set (just exprs).
        // Parse first expression; the next token decides.
        if self.eat(&TokenKind::DoubleStar) {
            // {**d, ...} — dict with spread.
            let val = self.parse_ternary()?;
            let mut keys: Vec<Option<Expr>> = vec![None];
            let mut values = vec![val];
            while self.eat(&TokenKind::Comma) {
                if self.check(&TokenKind::RBrace) {
                    break;
                }
                if self.eat(&TokenKind::DoubleStar) {
                    keys.push(None);
                    values.push(self.parse_ternary()?);
                } else {
                    let k = self.parse_ternary()?;
                    self.expect(&TokenKind::Colon, "`:`")?;
                    let v = self.parse_ternary()?;
                    keys.push(Some(k));
                    values.push(v);
                }
            }
            let rb = self.expect(&TokenKind::RBrace, "`}`")?;
            return Ok(Expr {
                kind: ExprKind::Dict { keys, values },
                span: lb.span.merge(rb.span),
            });
        }
        let first = self.parse_ternary()?;
        if self.eat(&TokenKind::Colon) {
            // Dict literal (or dict comprehension).
            let v = self.parse_ternary()?;
            if self.at_keyword(Keyword::For) {
                let generators = self.parse_comp_for()?;
                let rb = self.expect(&TokenKind::RBrace, "`}`")?;
                return Ok(Expr {
                    kind: ExprKind::DictComp {
                        key: Box::new(first),
                        value: Box::new(v),
                        generators,
                    },
                    span: lb.span.merge(rb.span),
                });
            }
            let mut keys: Vec<Option<Expr>> = vec![Some(first)];
            let mut values = vec![v];
            while self.eat(&TokenKind::Comma) {
                if self.check(&TokenKind::RBrace) {
                    break;
                }
                if self.eat(&TokenKind::DoubleStar) {
                    keys.push(None);
                    values.push(self.parse_ternary()?);
                } else {
                    let k = self.parse_ternary()?;
                    self.expect(&TokenKind::Colon, "`:`")?;
                    let vv = self.parse_ternary()?;
                    keys.push(Some(k));
                    values.push(vv);
                }
            }
            let rb = self.expect(&TokenKind::RBrace, "`}`")?;
            return Ok(Expr {
                kind: ExprKind::Dict { keys, values },
                span: lb.span.merge(rb.span),
            });
        }
        // Otherwise: set literal or set comp.
        if self.at_keyword(Keyword::For) {
            let generators = self.parse_comp_for()?;
            let rb = self.expect(&TokenKind::RBrace, "`}`")?;
            return Ok(Expr {
                kind: ExprKind::SetComp {
                    elt: Box::new(first),
                    generators,
                },
                span: lb.span.merge(rb.span),
            });
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.check(&TokenKind::RBrace) {
                break;
            }
            items.push(self.parse_ternary()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Expr {
            kind: ExprKind::Set(items),
            span: lb.span.merge(rb.span),
        })
    }

    fn parse_comp_for(&mut self) -> Result<Vec<Comprehension>, ParseError> {
        let mut generators = Vec::new();
        while self.at_keyword(Keyword::For) {
            self.bump();
            let target = self.parse_target_list_no_tuple()?;
            if !self.at_keyword(Keyword::In) {
                return Err(ParseError::Unexpected {
                    span: self.peek_token().span,
                    message: "expected `in` in comprehension".to_owned(),
                });
            }
            self.bump();
            let iter = self.parse_or()?;
            let mut ifs = Vec::new();
            while self.at_keyword(Keyword::If) {
                self.bump();
                ifs.push(self.parse_or()?);
            }
            generators.push(Comprehension {
                target,
                iter,
                ifs,
                is_async: false,
            });
        }
        if generators.is_empty() {
            return Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: "expected `for` in comprehension".to_owned(),
            });
        }
        Ok(generators)
    }

    fn parse_target_list_no_tuple(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_unary()?;
        if !self.check(&TokenKind::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at_keyword(Keyword::In) {
                break;
            }
            items.push(self.parse_unary()?);
        }
        let span = items[0].span.merge(items.last().unwrap().span);
        Ok(Expr {
            kind: ExprKind::Tuple(items),
            span,
        })
    }

    // ---------- string / number decoding ----------

    fn decode_string(&self, tok: &Token) -> Result<Constant, ParseError> {
        let lex = self.lexeme(tok.span);
        let (prefix_str, rest) = split_string_prefix(lex);
        let prefix = weavepy_lexer::StringPrefix::parse(prefix_str).ok_or_else(|| {
            ParseError::Unexpected {
                span: tok.span,
                message: format!("invalid string prefix {prefix_str:?}"),
            }
        })?;
        if prefix.fstring {
            return Err(ParseError::NotImplemented {
                span: tok.span,
                feature: "f-string",
                rfc: "RFC 0005",
            });
        }
        let body = strip_quotes(rest);
        let raw = prefix.raw;
        if prefix.bytes {
            let bytes = decode_bytes_body(body, raw).map_err(|m| ParseError::Unexpected {
                span: tok.span,
                message: m,
            })?;
            return Ok(Constant::Bytes(bytes));
        }
        let s = decode_str_body(body, raw).map_err(|m| ParseError::Unexpected {
            span: tok.span,
            message: m,
        })?;
        Ok(Constant::Str(s))
    }
}

/// Map a keyword to the follow-up RFC that tracks its support.
fn rfc_for(kw: Keyword) -> &'static str {
    match kw {
        Keyword::Class => "RFC 0003",
        Keyword::Try | Keyword::Except | Keyword::Finally | Keyword::Raise | Keyword::With => {
            "RFC 0004"
        }
        Keyword::Async | Keyword::Await | Keyword::Yield => "RFC 0006",
        _ => "RFC 0001",
    }
}

fn split_string_prefix(lex: &str) -> (&str, &str) {
    let mut idx = 0;
    for (i, c) in lex.char_indices() {
        if c == '"' || c == '\'' {
            idx = i;
            break;
        }
    }
    lex.split_at(idx)
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 6
        && ((bytes.starts_with(b"\"\"\"") && bytes.ends_with(b"\"\"\""))
            || (bytes.starts_with(b"'''") && bytes.ends_with(b"'''")))
    {
        &s[3..s.len() - 3]
    } else if bytes.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        ""
    }
}

fn decode_str_body(s: &str, raw: bool) -> Result<String, String> {
    if raw {
        return Ok(s.to_owned());
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(esc) = chars.next() else {
            out.push('\\');
            break;
        };
        match esc {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '0' => out.push('\0'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            '\n' => {} // line continuation inside string
            'x' => {
                let h1 = chars.next().ok_or("incomplete \\x escape")?;
                let h2 = chars.next().ok_or("incomplete \\x escape")?;
                let hex = format!("{h1}{h2}");
                let n = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
            }
            'u' => {
                let mut hex = String::new();
                for _ in 0..4 {
                    hex.push(chars.next().ok_or("incomplete \\u escape")?);
                }
                let n = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
            }
            other => {
                // CPython issues a DeprecationWarning for unknown
                // escapes but emits both characters literally.
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

fn decode_bytes_body(s: &str, raw: bool) -> Result<Vec<u8>, String> {
    if raw {
        return Ok(s.as_bytes().to_vec());
    }
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c.is_ascii() {
            if c != '\\' {
                out.push(c as u8);
                continue;
            }
        } else {
            return Err("non-ascii character in bytes literal".to_owned());
        }
        let Some(esc) = chars.next() else {
            out.push(b'\\');
            break;
        };
        match esc {
            'n' => out.push(b'\n'),
            'r' => out.push(b'\r'),
            't' => out.push(b'\t'),
            '\\' => out.push(b'\\'),
            '\'' => out.push(b'\''),
            '"' => out.push(b'"'),
            '0' => out.push(0),
            'a' => out.push(0x07),
            'b' => out.push(0x08),
            'f' => out.push(0x0c),
            'v' => out.push(0x0b),
            '\n' => {}
            'x' => {
                let h1 = chars.next().ok_or("incomplete \\x escape")?;
                let h2 = chars.next().ok_or("incomplete \\x escape")?;
                let hex = format!("{h1}{h2}");
                let n = u8::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                out.push(n);
            }
            other => {
                out.push(b'\\');
                if other.is_ascii() {
                    out.push(other as u8);
                }
            }
        }
    }
    Ok(out)
}

fn parse_number(lex: &str) -> Result<Constant, String> {
    let cleaned: String = lex.chars().filter(|c| *c != '_').collect();
    if cleaned.ends_with('j') || cleaned.ends_with('J') {
        return Err("complex numbers not supported in slice (see RFC 0001)".to_owned());
    }
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        let n = i64::from_str_radix(rest, 16).map_err(|e| e.to_string())?;
        return Ok(Constant::Int(n));
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        let n = i64::from_str_radix(rest, 8).map_err(|e| e.to_string())?;
        return Ok(Constant::Int(n));
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        let n = i64::from_str_radix(rest, 2).map_err(|e| e.to_string())?;
        return Ok(Constant::Int(n));
    }
    let has_float_marker = cleaned.contains('.') || cleaned.contains('e') || cleaned.contains('E');
    if has_float_marker {
        let f: f64 = cleaned
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())?;
        return Ok(Constant::Float(f));
    }
    let n: i64 = cleaned
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    Ok(Constant::Int(n))
}
