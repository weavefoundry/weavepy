//! Recursive-descent parser for Python source.
//!
//! Consumes the token stream from [`weavepy_lexer::tokenize`] and
//! produces a [`Module`]. Expression precedence follows the Python
//! language reference; chained comparisons (`a < b < c`) are
//! collapsed into a single [`Compare`] node like CPython does.
//!
//! The parser is hand-written so we own diagnostics end-to-end. The
//! feature set tracks RFC 0001 plus its follow-ups: classes (RFC 0003),
//! exceptions and `with` (RFC 0004), f-strings (RFC 0005), generators
//! (RFC 0006), pattern matching (RFC 0009), and imports (RFC 0012).
//! `async def`/`await` (RFC 0006-B) and PEP 701 nested-quote f-strings
//! (RFC 0005-B) remain explicit `ParseError::NotImplemented`s.

use weavepy_lexer::{Keyword, Span, Token, TokenKind};

use crate::ast::{
    Alias, Arg, Arguments, BinOp, BoolOp, CmpOp, Comprehension, Constant, ExceptHandler, Expr,
    ExprKind, Keyword as KwArg, MatchCase, Module, Pattern, Stmt, StmtKind, UnaryOp, WithItem,
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
        // Strip non-significant newlines and comments up front. The
        // lexer emits `Nl` tokens for physical newlines inside
        // brackets so explicit `\` continuations remain a syntactic
        // option; the parser never needs them as discrete tokens,
        // and removing them lets every collection / call site span
        // multiple lines without bespoke trivia handling.
        let tokens = tokens
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Nl | TokenKind::Comment))
            .collect();
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
        // Decorators apply to the next `def` or `class`.
        if matches!(self.peek(), TokenKind::At) {
            let decorators = self.parse_decorators()?;
            self.skip_trivia();
            return match self.peek() {
                TokenKind::Keyword(Keyword::Def) => self.parse_function_def(decorators),
                TokenKind::Keyword(Keyword::Class) => self.parse_class_def(decorators),
                other => Err(ParseError::Unexpected {
                    span: self.peek_token().span,
                    message: format!("expected `def` or `class` after decorator, got {other:?}"),
                }),
            };
        }
        match self.peek() {
            TokenKind::Keyword(kw) => match kw {
                Keyword::Def => self.parse_function_def(Vec::new()),
                Keyword::Class => self.parse_class_def(Vec::new()),
                Keyword::If => self.parse_if(),
                Keyword::While => self.parse_while(),
                Keyword::For => self.parse_for(),
                Keyword::Return => self.parse_return(),
                Keyword::Pass => self.simple_keyword_stmt(StmtKind::Pass),
                Keyword::Break => self.simple_keyword_stmt(StmtKind::Break),
                Keyword::Continue => self.simple_keyword_stmt(StmtKind::Continue),
                Keyword::Del => self.parse_del(),
                Keyword::Import => self.parse_import(),
                Keyword::From => self.parse_import_from(),
                Keyword::Global => self.parse_global(),
                Keyword::Nonlocal => self.parse_nonlocal(),
                Keyword::Try => self.parse_try(),
                Keyword::Raise => self.parse_raise(),
                Keyword::With => self.parse_with(),
                // `yield` at statement start parses as an expression
                // statement so the AST is `Expr(Yield(...))`, matching
                // CPython's lowering.
                Keyword::Yield => self.parse_simple_statement(),
                Keyword::Async | Keyword::Await => Err(ParseError::NotImplemented {
                    span: self.peek_token().span,
                    feature: kw.as_str(),
                    rfc: "RFC 0006-B",
                }),
                _ => self.parse_simple_statement(),
            },
            // `match` is a soft keyword — only treated as the statement
            // when followed by an expression, a `:`, and indented `case`.
            TokenKind::Name if self.lexeme(self.peek_token().span) == "match" => {
                if self.looks_like_match_statement() {
                    self.parse_match()
                } else {
                    self.parse_simple_statement()
                }
            }
            _ => self.parse_simple_statement(),
        }
    }

    /// `match` is a soft keyword. The disambiguating signal is
    /// `match <expr>:` followed by NEWLINE INDENT `case`. We look
    /// ahead conservatively — if any of those signals is missing,
    /// we fall back to treating `match` as an identifier.
    fn looks_like_match_statement(&self) -> bool {
        // Skip past `match`.
        let mut i = self.pos + 1;
        // Any non-end-of-statement token is a candidate for the subject.
        match self.tokens.get(i).map(|t| &t.kind) {
            Some(TokenKind::Newline)
            | Some(TokenKind::Semi)
            | Some(TokenKind::Endmarker)
            | Some(TokenKind::Equal)
            | Some(TokenKind::PlusEqual)
            | Some(TokenKind::MinusEqual)
            | Some(TokenKind::StarEqual)
            | Some(TokenKind::SlashEqual)
            | Some(TokenKind::DoubleSlashEqual)
            | Some(TokenKind::PercentEqual)
            | Some(TokenKind::AmperEqual)
            | Some(TokenKind::VbarEqual)
            | Some(TokenKind::CaretEqual)
            | Some(TokenKind::LeftShiftEqual)
            | Some(TokenKind::RightShiftEqual)
            | Some(TokenKind::DoubleStarEqual)
            | Some(TokenKind::AtEqual)
            | Some(TokenKind::ColonEqual)
            | Some(TokenKind::Dot)
            | Some(TokenKind::LPar)
            | Some(TokenKind::LSqb) => return false,
            None => return false,
            _ => {}
        }
        // Scan for a `:` at depth 0 followed by NEWLINE then `case`.
        let mut depth = 0i32;
        while let Some(tok) = self.tokens.get(i) {
            match &tok.kind {
                TokenKind::LPar | TokenKind::LSqb | TokenKind::LBrace => depth += 1,
                TokenKind::RPar | TokenKind::RSqb | TokenKind::RBrace => depth -= 1,
                TokenKind::Newline | TokenKind::Endmarker => return false,
                TokenKind::Colon if depth == 0 => {
                    // Look for NEWLINE (NL|COMMENT)* INDENT (NL|COMMENT)* `case`.
                    let mut j = i + 1;
                    if !matches!(
                        self.tokens.get(j).map(|t| &t.kind),
                        Some(TokenKind::Newline)
                    ) {
                        return false;
                    }
                    j += 1;
                    while matches!(
                        self.tokens.get(j).map(|t| &t.kind),
                        Some(TokenKind::Nl | TokenKind::Comment | TokenKind::Newline)
                    ) {
                        j += 1;
                    }
                    if !matches!(self.tokens.get(j).map(|t| &t.kind), Some(TokenKind::Indent)) {
                        return false;
                    }
                    j += 1;
                    while matches!(
                        self.tokens.get(j).map(|t| &t.kind),
                        Some(TokenKind::Nl | TokenKind::Comment)
                    ) {
                        j += 1;
                    }
                    return matches!(
                        self.tokens.get(j).map(|t| (&t.kind, self.lexeme(t.span))),
                        Some((TokenKind::Name, "case"))
                    );
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn parse_decorators(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut decorators = Vec::new();
        while matches!(self.peek(), TokenKind::At) {
            self.bump();
            let e = self.parse_expression(false)?;
            // After the decorator expression, consume a NEWLINE (and any
            // trivia leading to the next decorator or the def/class).
            self.consume_stmt_end()?;
            self.skip_trivia_and_newlines();
            decorators.push(e);
        }
        Ok(decorators)
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

    fn parse_function_def(&mut self, decorator_list: Vec<Expr>) -> Result<Stmt, ParseError> {
        let def_tok = self.bump(); // `def`
        let name_tok = self.expect(&TokenKind::Name, "function name")?;
        let name = self.lexeme(name_tok.span).to_owned();
        self.expect(&TokenKind::LPar, "`(`")?;
        let args = self.parse_function_arguments()?;
        self.expect(&TokenKind::RPar, "`)`")?;
        if self.eat(&TokenKind::RArrow) {
            let _ = self.parse_expression(false)?;
        }
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let span_end = body.last().map_or(def_tok.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
            },
            span: def_tok.span.merge(span_end),
        })
    }

    fn parse_class_def(&mut self, decorator_list: Vec<Expr>) -> Result<Stmt, ParseError> {
        let class_tok = self.bump(); // `class`
        let name_tok = self.expect(&TokenKind::Name, "class name")?;
        let name = self.lexeme(name_tok.span).to_owned();
        let (bases, keywords) = if self.eat(&TokenKind::LPar) {
            let (a, kw) = self.parse_call_args()?;
            self.expect(&TokenKind::RPar, "`)`")?;
            (a, kw)
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let span_end = body.last().map_or(class_tok.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
            },
            span: class_tok.span.merge(span_end),
        })
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        let try_tok = self.bump(); // `try`
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        self.skip_trivia_and_newlines();
        let mut handlers = Vec::new();
        let mut orelse: Vec<Stmt> = Vec::new();
        let mut finalbody: Vec<Stmt> = Vec::new();
        while self.at_keyword(Keyword::Except) {
            let exc_tok = self.bump(); // `except`
            if matches!(self.peek(), TokenKind::Star) {
                return Err(ParseError::NotImplemented {
                    span: self.peek_token().span,
                    feature: "except*",
                    rfc: "RFC 0011",
                });
            }
            let (type_, name) = if self.check(&TokenKind::Colon) {
                (None, None)
            } else {
                let t = self.parse_expression(false)?;
                let n = if self.at_keyword(Keyword::As) {
                    self.bump();
                    let nt = self.expect(&TokenKind::Name, "name after `as`")?;
                    Some(self.lexeme(nt.span).to_owned())
                } else {
                    None
                };
                (Some(t), n)
            };
            self.expect(&TokenKind::Colon, "`:`")?;
            let handler_body = self.parse_block()?;
            let span_end = handler_body.last().map_or(exc_tok.span, |s| s.span);
            handlers.push(ExceptHandler {
                type_,
                name,
                body: handler_body,
                span: exc_tok.span.merge(span_end),
            });
            self.skip_trivia_and_newlines();
        }
        if self.at_keyword(Keyword::Else) {
            self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            orelse = self.parse_block()?;
            self.skip_trivia_and_newlines();
        }
        if self.at_keyword(Keyword::Finally) {
            self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            finalbody = self.parse_block()?;
        }
        if handlers.is_empty() && finalbody.is_empty() {
            return Err(ParseError::Unexpected {
                span: try_tok.span,
                message: "expected `except` or `finally` after `try`".to_owned(),
            });
        }
        let span_end = finalbody
            .last()
            .or_else(|| orelse.last())
            .or_else(|| handlers.last().map(|h| &h.body).and_then(|b| b.last()))
            .or_else(|| body.last())
            .map_or(try_tok.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            },
            span: try_tok.span.merge(span_end),
        })
    }

    fn parse_raise(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump(); // `raise`
        let (exc, cause) = if matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semi | TokenKind::Endmarker
        ) {
            (None, None)
        } else {
            let e = self.parse_expression(false)?;
            let c = if self.at_keyword(Keyword::From) {
                self.bump();
                Some(self.parse_expression(false)?)
            } else {
                None
            };
            (Some(e), c)
        };
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Raise { exc, cause },
            span: kw.span,
        })
    }

    fn parse_with(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump(); // `with`
                              // CPython 3.10+ supports `with (a, b as c, d): body` with
                              // parenthesized item lists. The slice supports both forms.
        let mut items = Vec::new();
        let paren = matches!(self.peek(), TokenKind::LPar) && self.is_with_paren_list_start();
        if paren {
            self.bump();
            loop {
                items.push(self.parse_with_item()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if self.check(&TokenKind::RPar) {
                    break;
                }
            }
            self.expect(&TokenKind::RPar, "`)`")?;
        } else {
            loop {
                items.push(self.parse_with_item()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let span_end = body.last().map_or(kw.span, |s| s.span);
        Ok(Stmt {
            kind: StmtKind::With { items, body },
            span: kw.span.merge(span_end),
        })
    }

    /// Lookahead: is the `(` we just saw the start of a parenthesised
    /// `with`-item list (rather than a parenthesised expression like
    /// `with (a + b): ...`)? Heuristic: scan forward for `as` or `,`
    /// at depth 0 before the closing `)` and a `:`.
    fn is_with_paren_list_start(&self) -> bool {
        let mut depth = 0i32;
        let mut i = self.pos;
        while let Some(tok) = self.tokens.get(i) {
            match &tok.kind {
                TokenKind::LPar | TokenKind::LSqb | TokenKind::LBrace => depth += 1,
                TokenKind::RPar | TokenKind::RSqb | TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                TokenKind::Comma if depth == 1 => return true,
                TokenKind::Keyword(Keyword::As) if depth == 1 => return true,
                TokenKind::Newline | TokenKind::Endmarker => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn parse_with_item(&mut self) -> Result<WithItem, ParseError> {
        let context_expr = self.parse_expression(false)?;
        let optional_vars = if self.at_keyword(Keyword::As) {
            self.bump();
            Some(self.parse_unary()?)
        } else {
            None
        };
        Ok(WithItem {
            context_expr,
            optional_vars,
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
                // Tolerate a trailing comma inside parenthesised
                // `from x import (a, b,)` — common in real codebases.
                if paren && matches!(self.peek(), TokenKind::RPar) {
                    break;
                }
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

    /// `del target_list`. Each target is any assignable expression; we
    /// reuse `parse_ternary` so subscripts and attribute access are
    /// supported with no extra plumbing.
    fn parse_del(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let mut targets = vec![self.parse_ternary()?];
        while self.eat(&TokenKind::Comma) {
            if matches!(
                self.peek(),
                TokenKind::Newline | TokenKind::Semi | TokenKind::Endmarker
            ) {
                break;
            }
            targets.push(self.parse_ternary()?);
        }
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Delete(targets),
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

    // ---------- match / case ----------

    /// `match subject: NEWLINE INDENT (case_block)+ DEDENT`. Caller has
    /// confirmed via [`Self::looks_like_match_statement`] that the
    /// soft-keyword `match` is in fact starting a match statement.
    fn parse_match(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let subject = self.parse_match_subject()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        self.expect(&TokenKind::Newline, "newline after `match ... :`")?;
        self.skip_trivia_and_newlines();
        self.expect(&TokenKind::Indent, "indented block")?;

        let mut cases = Vec::new();
        loop {
            self.skip_trivia_and_newlines();
            if matches!(self.peek(), TokenKind::Dedent | TokenKind::Endmarker) {
                break;
            }
            cases.push(self.parse_case_clause()?);
        }
        self.eat(&TokenKind::Dedent);
        if cases.is_empty() {
            return Err(ParseError::Unexpected {
                span: kw.span,
                message: "match statement needs at least one case".to_owned(),
            });
        }
        let span_end = cases.last().map_or(kw.span, |c| c.span);
        Ok(Stmt {
            kind: StmtKind::Match { subject, cases },
            span: kw.span.merge(span_end),
        })
    }

    /// CPython allows `match a, b:` (subject is an implicit tuple). We
    /// follow.
    fn parse_match_subject(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_ternary()?;
        if !self.check(&TokenKind::Comma) {
            return Ok(first);
        }
        let start_span = first.span;
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.check(&TokenKind::Colon) {
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

    fn parse_case_clause(&mut self) -> Result<MatchCase, ParseError> {
        // The contextual keyword `case` is a `Name` token.
        let case_tok = self.peek_token().clone();
        if !(matches!(self.peek(), TokenKind::Name) && self.lexeme(case_tok.span) == "case") {
            return Err(ParseError::Unexpected {
                span: case_tok.span,
                message: "expected `case`".to_owned(),
            });
        }
        self.bump();
        let pattern = self.parse_pattern()?;
        let guard = if self.at_keyword(Keyword::If) {
            self.bump();
            Some(self.parse_expression(false)?)
        } else {
            None
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block()?;
        let span_end = body.last().map_or(case_tok.span, |s| s.span);
        Ok(MatchCase {
            pattern,
            guard,
            body,
            span: case_tok.span.merge(span_end),
        })
    }

    /// Top-level pattern: `or_pattern ('as' NAME)?`.
    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let pat = self.parse_or_pattern()?;
        if self.at_keyword(Keyword::As) {
            self.bump();
            let n = self.expect(&TokenKind::Name, "name after `as`")?;
            let name = self.lexeme(n.span).to_owned();
            if name == "_" {
                return Err(ParseError::Unexpected {
                    span: n.span,
                    message: "cannot use `_` as a capture target".to_owned(),
                });
            }
            return Ok(Pattern::As {
                pattern: Box::new(pat),
                name,
            });
        }
        Ok(pat)
    }

    /// `or_pattern: closed_pattern ('|' closed_pattern)*`.
    fn parse_or_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.parse_closed_pattern()?;
        if !self.check(&TokenKind::Vbar) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&TokenKind::Vbar) {
            alts.push(self.parse_closed_pattern()?);
        }
        Ok(Pattern::Or(alts))
    }

    /// One non-alternation pattern: literal, name, sequence, mapping,
    /// class, parenthesized, etc.
    fn parse_closed_pattern(&mut self) -> Result<Pattern, ParseError> {
        // Star in sequence: `[a, *rest]` or `*_`.
        if self.check(&TokenKind::Star) {
            self.bump();
            let name = match self.peek() {
                TokenKind::Name => {
                    let tok = self.bump();
                    let s = self.lexeme(tok.span).to_owned();
                    if s == "_" {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => {
                    return Err(ParseError::Unexpected {
                        span: self.peek_token().span,
                        message: "expected name after `*` in pattern".to_owned(),
                    });
                }
            };
            return Ok(Pattern::Star(name));
        }
        // Numeric / string / singleton literal patterns. `-N` is
        // allowed (negative numeric literal pattern).
        if matches!(
            self.peek(),
            TokenKind::Number | TokenKind::String | TokenKind::Minus
        ) {
            let e = self.parse_literal_pattern_expr()?;
            return Ok(Pattern::Value(e));
        }
        if self.at_keyword(Keyword::None) {
            self.bump();
            return Ok(Pattern::Singleton(Constant::None));
        }
        if self.at_keyword(Keyword::True) {
            self.bump();
            return Ok(Pattern::Singleton(Constant::Bool(true)));
        }
        if self.at_keyword(Keyword::False) {
            self.bump();
            return Ok(Pattern::Singleton(Constant::Bool(false)));
        }
        if self.check(&TokenKind::LSqb) {
            return self.parse_sequence_pattern(true);
        }
        if self.check(&TokenKind::LPar) {
            return self.parse_paren_or_tuple_pattern();
        }
        if self.check(&TokenKind::LBrace) {
            return self.parse_mapping_pattern();
        }
        // Identifier-led: capture, wildcard, or value (qualified name)
        // — and possibly a class pattern if `(` follows.
        if matches!(self.peek(), TokenKind::Name) {
            return self.parse_name_pattern();
        }
        Err(ParseError::Unexpected {
            span: self.peek_token().span,
            message: format!("unexpected token in pattern: {:?}", self.peek()),
        })
    }

    /// Parse an expression that appears as the *value* of a literal
    /// pattern. Restricted to numbers, strings, and unary `-` on
    /// numerics — matching PEP 634.
    fn parse_literal_pattern_expr(&mut self) -> Result<Expr, ParseError> {
        if self.check(&TokenKind::Minus) {
            let minus = self.bump();
            let tok = self.peek_token().clone();
            if !matches!(tok.kind, TokenKind::Number) {
                return Err(ParseError::Unexpected {
                    span: tok.span,
                    message: "expected numeric literal after `-` in pattern".to_owned(),
                });
            }
            self.bump();
            let value =
                parse_number(self.lexeme(tok.span)).map_err(|m| ParseError::Unexpected {
                    span: tok.span,
                    message: m,
                })?;
            let value = match value {
                Constant::Int(i) => Constant::Int(-i),
                Constant::Float(f) => Constant::Float(-f),
                other => other,
            };
            return Ok(Expr {
                kind: ExprKind::Constant(value),
                span: minus.span.merge(tok.span),
            });
        }
        self.parse_atom()
    }

    /// `Name (. Name)* ('(' pat_args ')')?`. The `.`-chain makes it a
    /// value pattern; `(` makes it a class pattern; otherwise capture.
    fn parse_name_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.bump();
        let first_name = self.lexeme(first.span).to_owned();
        // Dotted: value pattern.
        if self.check(&TokenKind::Dot) {
            let mut expr = Expr {
                kind: ExprKind::Name(first_name),
                span: first.span,
            };
            while self.eat(&TokenKind::Dot) {
                let n = self.expect(&TokenKind::Name, "attribute name in pattern")?;
                let attr = self.lexeme(n.span).to_owned();
                let span = expr.span.merge(n.span);
                expr = Expr {
                    kind: ExprKind::Attribute {
                        value: Box::new(expr),
                        attr,
                    },
                    span,
                };
            }
            if self.check(&TokenKind::LPar) {
                return self.finish_class_pattern(expr);
            }
            return Ok(Pattern::Value(expr));
        }
        // Class pattern: bare `Name(...)`.
        if self.check(&TokenKind::LPar) {
            let cls = Expr {
                kind: ExprKind::Name(first_name),
                span: first.span,
            };
            return self.finish_class_pattern(cls);
        }
        // Wildcard `_` binds nothing.
        if first_name == "_" {
            return Ok(Pattern::Capture(None));
        }
        Ok(Pattern::Capture(Some(first_name)))
    }

    fn finish_class_pattern(&mut self, cls: Expr) -> Result<Pattern, ParseError> {
        self.expect(&TokenKind::LPar, "`(`")?;
        let mut positionals = Vec::new();
        let mut keywords: Vec<(String, Pattern)> = Vec::new();
        let mut saw_kw = false;
        while !self.check(&TokenKind::RPar) {
            // A keyword arg: `name=pattern`.
            if matches!(self.peek(), TokenKind::Name)
                && matches!(self.peek_at(1), Some(TokenKind::Equal))
            {
                let n = self.bump();
                let name = self.lexeme(n.span).to_owned();
                self.bump(); // `=`
                let p = self.parse_pattern()?;
                keywords.push((name, p));
                saw_kw = true;
            } else {
                if saw_kw {
                    return Err(ParseError::Unexpected {
                        span: self.peek_token().span,
                        message: "positional pattern after keyword pattern".to_owned(),
                    });
                }
                positionals.push(self.parse_pattern()?);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RPar, "`)`")?;
        Ok(Pattern::Class {
            cls,
            positionals,
            keywords,
        })
    }

    fn parse_sequence_pattern(&mut self, square: bool) -> Result<Pattern, ParseError> {
        let close = if square {
            TokenKind::RSqb
        } else {
            TokenKind::RPar
        };
        self.bump();
        let mut items = Vec::new();
        while !self.check(&close) {
            items.push(self.parse_pattern()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&close, if square { "`]`" } else { "`)`" })?;
        Ok(Pattern::Sequence(items))
    }

    /// `(p)` (parenthesized pattern, equivalent to `p`) or
    /// `(p, q, ...)` (tuple sequence pattern).
    fn parse_paren_or_tuple_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.bump();
        if self.eat(&TokenKind::RPar) {
            return Ok(Pattern::Sequence(Vec::new()));
        }
        let first = self.parse_pattern()?;
        if !self.check(&TokenKind::Comma) {
            self.expect(&TokenKind::RPar, "`)`")?;
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.check(&TokenKind::RPar) {
                break;
            }
            items.push(self.parse_pattern()?);
        }
        self.expect(&TokenKind::RPar, "`)`")?;
        Ok(Pattern::Sequence(items))
    }

    fn parse_mapping_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.bump();
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut rest: Option<Option<String>> = None;
        while !self.check(&TokenKind::RBrace) {
            if self.eat(&TokenKind::DoubleStar) {
                let n = self.expect(&TokenKind::Name, "name after `**` in mapping pattern")?;
                let name = self.lexeme(n.span).to_owned();
                rest = Some(if name == "_" { None } else { Some(name) });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            let key = self.parse_literal_or_value_key()?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let p = self.parse_pattern()?;
            keys.push(key);
            patterns.push(p);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Pattern::Mapping {
            keys,
            patterns,
            rest,
        })
    }

    /// Allowed mapping keys per PEP 634: numeric/string literals or
    /// dotted attribute expressions (value patterns).
    fn parse_literal_or_value_key(&mut self) -> Result<Expr, ParseError> {
        if matches!(
            self.peek(),
            TokenKind::Number | TokenKind::String | TokenKind::Minus
        ) {
            return self.parse_literal_pattern_expr();
        }
        if self.at_keyword(Keyword::None) {
            let t = self.bump();
            return Ok(Expr {
                kind: ExprKind::Constant(Constant::None),
                span: t.span,
            });
        }
        if self.at_keyword(Keyword::True) {
            let t = self.bump();
            return Ok(Expr {
                kind: ExprKind::Constant(Constant::Bool(true)),
                span: t.span,
            });
        }
        if self.at_keyword(Keyword::False) {
            let t = self.bump();
            return Ok(Expr {
                kind: ExprKind::Constant(Constant::Bool(false)),
                span: t.span,
            });
        }
        // Dotted name as a value key.
        let n = self.expect(&TokenKind::Name, "key")?;
        let mut expr = Expr {
            kind: ExprKind::Name(self.lexeme(n.span).to_owned()),
            span: n.span,
        };
        while self.eat(&TokenKind::Dot) {
            let attr_tok = self.expect(&TokenKind::Name, "attribute name in key")?;
            let attr = self.lexeme(attr_tok.span).to_owned();
            let span = expr.span.merge(attr_tok.span);
            expr = Expr {
                kind: ExprKind::Attribute {
                    value: Box::new(expr),
                    attr,
                },
                span,
            };
        }
        Ok(expr)
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
            // Inline single-statement block: `if x: y = 1`, `class A: pass`.
            let s = self.parse_statement()?;
            Ok(vec![s])
        }
    }

    // ============================================================
    // Expressions
    // ============================================================

    /// Parse one expression. If `allow_tuple` is true, top-level
    /// `,` builds a `Tuple`; if false, comma is a delimiter that
    /// ends the expression.
    fn parse_expression(&mut self, allow_tuple: bool) -> Result<Expr, ParseError> {
        if allow_tuple {
            self.parse_expression_list(true)
        } else {
            self.parse_ternary()
        }
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
        // `yield` and `yield from` are expressions in CPython's grammar.
        // They're only legal inside function bodies; the compiler — not
        // the parser — enforces that.
        if self.at_keyword(Keyword::Yield) {
            return self.parse_yield();
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

    fn parse_yield(&mut self) -> Result<Expr, ParseError> {
        let kw = self.bump(); // `yield`
        if self.at_keyword(Keyword::From) {
            self.bump();
            let value = self.parse_ternary()?;
            let span = kw.span.merge(value.span);
            return Ok(Expr {
                kind: ExprKind::YieldFrom(Box::new(value)),
                span,
            });
        }
        // Bare `yield` followed by an end-of-expression token returns
        // `Yield(value=None)`. Otherwise parse a single value (or
        // implicit-tuple `yield 1, 2`).
        if matches!(
            self.peek(),
            TokenKind::Newline
                | TokenKind::Semi
                | TokenKind::Endmarker
                | TokenKind::RPar
                | TokenKind::RSqb
                | TokenKind::RBrace
                | TokenKind::Comma
                | TokenKind::Colon
                | TokenKind::Equal
        ) {
            return Ok(Expr {
                kind: ExprKind::Yield(None),
                span: kw.span,
            });
        }
        let value = self.parse_expression_list(true)?;
        let span = kw.span.merge(value.span);
        Ok(Expr {
            kind: ExprKind::Yield(Some(Box::new(value))),
            span,
        })
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
        // Parse a single (sub)slice (`a`, `a:b`, `a:b:c`, etc.) — the
        // comma-separated form (`x[a, b, c]`) is handled by the outer
        // loop after this call returns.
        let first = self.parse_subscript_single()?;
        if !self.check(&TokenKind::Comma) {
            return Ok(first);
        }
        // Multi-element subscript: collect into a tuple. Used by
        // generic typing (`Dict[K, V]`, `Tuple[A, B, C]`), and by
        // NumPy-style indexing (`arr[i, j]`).
        let mut elts = vec![first];
        while self.eat(&TokenKind::Comma) {
            if matches!(self.peek(), TokenKind::RSqb) {
                break;
            }
            elts.push(self.parse_subscript_single()?);
        }
        let span = elts
            .first()
            .map(|e| e.span)
            .unwrap_or_else(|| self.peek_token().span);
        Ok(Expr {
            kind: ExprKind::Tuple(elts),
            span,
        })
    }

    fn parse_subscript_single(&mut self) -> Result<Expr, ParseError> {
        // Slice grammar: `lower? ':' upper? (':' step?)?` or plain expr.
        if self.check(&TokenKind::Colon) {
            self.bump();
            let upper = if matches!(
                self.peek(),
                TokenKind::Colon | TokenKind::RSqb | TokenKind::Comma
            ) {
                None
            } else {
                Some(Box::new(self.parse_ternary()?))
            };
            let step = if self.eat(&TokenKind::Colon) {
                if matches!(self.peek(), TokenKind::RSqb | TokenKind::Comma) {
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
        self.bump();
        let upper = if matches!(
            self.peek(),
            TokenKind::Colon | TokenKind::RSqb | TokenKind::Comma
        ) {
            None
        } else {
            Some(Box::new(self.parse_ternary()?))
        };
        let step = if self.eat(&TokenKind::Colon) {
            if matches!(self.peek(), TokenKind::RSqb | TokenKind::Comma) {
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
            TokenKind::String => self.parse_string_concat(tok),
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

    /// Handle adjacent-string concatenation, mixing plain strings,
    /// byte strings, and f-strings. The CPython AST flattens these:
    /// `"a" "b"` → `Constant("ab")`, `f"a" "b"` → `JoinedStr(["ab"])`,
    /// `f"a{x}" "b"` → `JoinedStr(["a", FormattedValue(x), "b"])`.
    fn parse_string_concat(&mut self, first: Token) -> Result<Expr, ParseError> {
        let mut span = first.span;
        let first_prefix = self.string_prefix(&first)?;
        let mut accum: AccumString = if first_prefix.fstring {
            AccumString::Joined(self.fstring_parts_for(&first)?)
        } else if first_prefix.bytes {
            match self.decode_string(&first)? {
                Constant::Bytes(b) => AccumString::Bytes(b),
                _ => unreachable!(),
            }
        } else {
            match self.decode_string(&first)? {
                Constant::Str(s) => AccumString::Plain(s),
                _ => unreachable!(),
            }
        };
        self.bump();
        while matches!(self.peek(), TokenKind::String) {
            let next_tok = self.peek_token().clone();
            let next_prefix = self.string_prefix(&next_tok)?;
            span = span.merge(next_tok.span);
            self.bump();
            accum = match (accum, next_prefix.fstring, next_prefix.bytes) {
                (AccumString::Bytes(mut a), false, true) => match self.decode_string(&next_tok)? {
                    Constant::Bytes(b) => {
                        a.extend_from_slice(&b);
                        AccumString::Bytes(a)
                    }
                    _ => unreachable!(),
                },
                (AccumString::Bytes(_), _, _) => {
                    return Err(ParseError::Unexpected {
                        span: next_tok.span,
                        message: "cannot concatenate str and bytes literals".to_owned(),
                    });
                }
                (_, _, true) => {
                    return Err(ParseError::Unexpected {
                        span: next_tok.span,
                        message: "cannot concatenate str and bytes literals".to_owned(),
                    });
                }
                (AccumString::Plain(mut a), false, false) => {
                    match self.decode_string(&next_tok)? {
                        Constant::Str(s) => {
                            a.push_str(&s);
                            AccumString::Plain(a)
                        }
                        _ => unreachable!(),
                    }
                }
                (AccumString::Plain(a), true, false) => {
                    let mut parts: Vec<Expr> = Vec::new();
                    if !a.is_empty() {
                        parts.push(Expr {
                            kind: ExprKind::Constant(Constant::Str(a)),
                            span: first.span,
                        });
                    }
                    parts.extend(self.fstring_parts_for(&next_tok)?);
                    AccumString::Joined(parts)
                }
                (AccumString::Joined(mut parts), false, false) => {
                    match self.decode_string(&next_tok)? {
                        Constant::Str(s) => {
                            join_str_into_parts(&mut parts, s, next_tok.span);
                            AccumString::Joined(parts)
                        }
                        _ => unreachable!(),
                    }
                }
                (AccumString::Joined(mut parts), true, false) => {
                    let new_parts = self.fstring_parts_for(&next_tok)?;
                    for p in new_parts {
                        if let ExprKind::Constant(Constant::Str(s)) = p.kind {
                            join_str_into_parts(&mut parts, s, p.span);
                        } else {
                            parts.push(p);
                        }
                    }
                    AccumString::Joined(parts)
                }
            };
        }
        match accum {
            AccumString::Plain(s) => Ok(Expr {
                kind: ExprKind::Constant(Constant::Str(s)),
                span,
            }),
            AccumString::Bytes(b) => Ok(Expr {
                kind: ExprKind::Constant(Constant::Bytes(b)),
                span,
            }),
            AccumString::Joined(parts) => {
                if parts.len() == 1 {
                    if matches!(parts[0].kind, ExprKind::Constant(_)) {
                        return Ok(Expr {
                            kind: parts[0].kind.clone(),
                            span,
                        });
                    }
                }
                Ok(Expr {
                    kind: ExprKind::JoinedStr(parts),
                    span,
                })
            }
        }
    }

    /// Decode one f-string token, flattening any nested `JoinedStr`
    /// produced by the debug `{x = }` shortcut into the outer parts list.
    fn fstring_parts_for(&self, tok: &Token) -> Result<Vec<Expr>, ParseError> {
        let parsed = self.parse_fstring_token(tok)?;
        let mut parts = Vec::new();
        match parsed.kind {
            ExprKind::JoinedStr(inner) => {
                for p in inner {
                    if let ExprKind::JoinedStr(more) = p.kind {
                        parts.extend(more);
                    } else {
                        parts.push(p);
                    }
                }
            }
            other => parts.push(Expr {
                kind: other,
                span: parsed.span,
            }),
        }
        Ok(parts)
    }

    // ---------- string / number decoding ----------

    /// Decode a single (non-f) string token into a [`Constant`].
    fn decode_string(&self, tok: &Token) -> Result<Constant, ParseError> {
        let lex = self.lexeme(tok.span);
        let (prefix_str, rest) = split_string_prefix(lex);
        let prefix = weavepy_lexer::StringPrefix::parse(prefix_str).ok_or_else(|| {
            ParseError::Unexpected {
                span: tok.span,
                message: format!("invalid string prefix {prefix_str:?}"),
            }
        })?;
        debug_assert!(
            !prefix.fstring,
            "f-strings should route through decode_string_or_fstring"
        );
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

    /// Returns the prefix info for a string token without decoding the body.
    fn string_prefix(&self, tok: &Token) -> Result<weavepy_lexer::StringPrefix, ParseError> {
        let lex = self.lexeme(tok.span);
        let (prefix_str, _) = split_string_prefix(lex);
        weavepy_lexer::StringPrefix::parse(prefix_str).ok_or_else(|| ParseError::Unexpected {
            span: tok.span,
            message: format!("invalid string prefix {prefix_str:?}"),
        })
    }

    /// Parse the interior of an f-string token into an `ExprKind`.
    ///
    /// Walks the body character-by-character. Literal runs become
    /// `Constant::Str` parts; `{...}` runs are re-lexed and re-parsed
    /// to produce `FormattedValue` parts. The result is a `JoinedStr`
    /// (or a single `Constant` when no interpolation appears).
    fn parse_fstring_token(&self, tok: &Token) -> Result<Expr, ParseError> {
        let lex = self.lexeme(tok.span);
        let (_, rest) = split_string_prefix(lex);
        let raw = self.string_prefix(tok)?.raw;
        let body = strip_quotes(rest);
        let parts = self.parse_fstring_body(body, raw, tok.span)?;
        if parts.len() == 1 {
            if let ExprKind::Constant(_) = &parts[0].kind {
                return Ok(parts[0].clone());
            }
        }
        Ok(Expr {
            kind: ExprKind::JoinedStr(parts),
            span: tok.span,
        })
    }

    fn parse_fstring_body(
        &self,
        body: &str,
        raw: bool,
        anchor: Span,
    ) -> Result<Vec<Expr>, ParseError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let bytes = body.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'{' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    literal.push('{');
                    i += 2;
                    continue;
                }
                if !literal.is_empty() {
                    let decoded =
                        decode_str_body(&literal, raw).map_err(|m| ParseError::Unexpected {
                            span: anchor,
                            message: m,
                        })?;
                    parts.push(Expr {
                        kind: ExprKind::Constant(Constant::Str(decoded)),
                        span: anchor,
                    });
                    literal.clear();
                }
                let (field, end) = self.scan_fstring_field(body, i + 1, anchor)?;
                let parsed = self.parse_fstring_field(&field, anchor)?;
                parts.push(parsed);
                i = end + 1; // skip past the closing `}`
                continue;
            }
            if b == b'}' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    literal.push('}');
                    i += 2;
                    continue;
                }
                return Err(ParseError::Unexpected {
                    span: anchor,
                    message: "single '}' is not allowed in f-string".to_owned(),
                });
            }
            // Append the next UTF-8 character (one or more bytes).
            let ch_len = utf8_char_len(b);
            let end = (i + ch_len).min(bytes.len());
            literal.push_str(&body[i..end]);
            i = end;
        }
        if !literal.is_empty() || parts.is_empty() {
            let decoded = decode_str_body(&literal, raw).map_err(|m| ParseError::Unexpected {
                span: anchor,
                message: m,
            })?;
            parts.push(Expr {
                kind: ExprKind::Constant(Constant::Str(decoded)),
                span: anchor,
            });
        }
        Ok(parts)
    }

    /// Scan from just past the opening `{` to the matching `}` at depth 0.
    /// Returns the field text and the index of the closing `}`.
    fn scan_fstring_field(
        &self,
        body: &str,
        start: usize,
        anchor: Span,
    ) -> Result<(String, usize), ParseError> {
        let bytes = body.as_bytes();
        let mut depth = 1i32;
        let mut i = start;
        // String state machine for backtick-free quotes inside the field.
        let mut in_str: Option<u8> = None;
        let mut triple = false;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = in_str {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == q {
                    if triple {
                        if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                            i += 3;
                            in_str = None;
                            triple = false;
                            continue;
                        }
                    } else {
                        i += 1;
                        in_str = None;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' => {
                    let q = b;
                    if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                        in_str = Some(q);
                        triple = true;
                        i += 3;
                    } else {
                        in_str = Some(q);
                        triple = false;
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    i += 1;
                }
                b')' | b']' => {
                    depth -= 1;
                    i += 1;
                }
                b'}' => {
                    if depth == 1 {
                        return Ok((body[start..i].to_owned(), i));
                    }
                    depth -= 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        Err(ParseError::Unexpected {
            span: anchor,
            message: "expected '}' to close f-string replacement field".to_owned(),
        })
    }

    /// Parse one `expr[!conv][:spec]` field and return a
    /// `FormattedValue` (possibly preceded by a synthetic literal
    /// for `{x = }` debug form).
    fn parse_fstring_field(&self, field: &str, anchor: Span) -> Result<Expr, ParseError> {
        // Split into expr, conversion, format_spec. Backslashes inside
        // an f-string field aren't allowed in CPython <3.12 — we
        // surface that as a parse error for clarity.
        if field.contains('\\') {
            return Err(ParseError::NotImplemented {
                span: anchor,
                feature: "backslashes inside f-string replacement fields",
                rfc: "RFC 0005-B",
            });
        }
        let bytes = field.as_bytes();
        // Find the `!conv` and `:spec` boundaries at top level (not
        // inside nested parens/brackets/braces or string literals).
        let mut expr_end = bytes.len();
        let mut conv_start: Option<usize> = None;
        let mut spec_start: Option<usize> = None;
        let mut depth = 0i32;
        let mut in_str: Option<u8> = None;
        let mut triple = false;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = in_str {
                if b == q {
                    if triple {
                        if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                            in_str = None;
                            triple = false;
                            i += 3;
                            continue;
                        }
                    } else {
                        in_str = None;
                        i += 1;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' => {
                    let q = b;
                    if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                        in_str = Some(q);
                        triple = true;
                        i += 3;
                        continue;
                    }
                    in_str = Some(q);
                    triple = false;
                    i += 1;
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                if b == b'!' && conv_start.is_none() && spec_start.is_none() {
                    // `!=` and `!<` etc. are comparison; `!` followed
                    // by `s` / `r` / `a` is conversion.
                    if i + 1 < bytes.len() && matches!(bytes[i + 1], b's' | b'r' | b'a') {
                        expr_end = i;
                        conv_start = Some(i + 1);
                        i += 2;
                        continue;
                    }
                } else if b == b':' && spec_start.is_none() {
                    expr_end = expr_end.min(i);
                    spec_start = Some(i + 1);
                    i += 1;
                    continue;
                }
            }
            i += 1;
        }
        let expr_text = &field[..expr_end];
        // Debug form `{x = }`: literal "x = " prepended, conversion
        // forced to `r` if no explicit conversion / spec.
        let (expr_text, debug_lit) = if expr_text.trim_end().ends_with('=') {
            let trimmed = expr_text.trim_end();
            let without_eq = trimmed.trim_end_matches('=');
            let literal = format!("{without_eq}=");
            (without_eq.trim(), Some(literal))
        } else {
            (expr_text.trim(), None)
        };
        if expr_text.is_empty() {
            return Err(ParseError::Unexpected {
                span: anchor,
                message: "empty expression in f-string replacement field".to_owned(),
            });
        }
        // Recursively tokenize+parse the expression.
        let tokens = weavepy_lexer::tokenize(expr_text)?;
        let mut sub = Parser::new(expr_text, tokens);
        sub.skip_trivia_and_newlines();
        let value = sub.parse_expression_list(false)?;
        sub.skip_trivia_and_newlines();
        if !matches!(sub.peek(), TokenKind::Endmarker) {
            return Err(ParseError::Unexpected {
                span: anchor,
                message: "trailing tokens in f-string expression".to_owned(),
            });
        }

        let conversion = match conv_start {
            Some(idx) => i32::from(field.as_bytes()[idx]),
            None if debug_lit.is_some() => i32::from(b'r'),
            None => -1,
        };
        let format_spec = match spec_start {
            Some(s) => {
                let spec = &field[s..];
                let inner = self.parse_fstring_body(spec, false, anchor)?;
                Some(Box::new(Expr {
                    kind: ExprKind::JoinedStr(inner),
                    span: anchor,
                }))
            }
            None => None,
        };

        let fv = Expr {
            kind: ExprKind::FormattedValue {
                value: Box::new(value),
                conversion,
                format_spec,
            },
            span: anchor,
        };
        if let Some(lit) = debug_lit {
            // Wrap in a tiny JoinedStr-equivalent: emit a literal
            // followed by the formatted value. The caller of
            // parse_fstring_field appends parts directly, so we
            // package both into a synthetic JoinedStr that will
            // get flattened by the outer JoinedStr later.
            return Ok(Expr {
                kind: ExprKind::JoinedStr(vec![
                    Expr {
                        kind: ExprKind::Constant(Constant::Str(lit)),
                        span: anchor,
                    },
                    fv,
                ]),
                span: anchor,
            });
        }
        Ok(fv)
    }
}

#[inline]
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b & 0b1110_0000 == 0b1100_0000 {
        2
    } else if b & 0b1111_0000 == 0b1110_0000 {
        3
    } else {
        4
    }
}

/// Working state while concatenating adjacent string tokens.
enum AccumString {
    Plain(String),
    Bytes(Vec<u8>),
    Joined(Vec<Expr>),
}

/// Append a literal string onto the tail of a JoinedStr parts list.
/// Merges with the trailing `Constant::Str` part if there is one.
fn join_str_into_parts(parts: &mut Vec<Expr>, s: String, span: Span) {
    if let Some(last) = parts.last_mut() {
        if let ExprKind::Constant(Constant::Str(existing)) = &mut last.kind {
            existing.push_str(&s);
            return;
        }
    }
    parts.push(Expr {
        kind: ExprKind::Constant(Constant::Str(s)),
        span,
    });
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
