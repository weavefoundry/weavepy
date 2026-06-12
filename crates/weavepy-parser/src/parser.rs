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

    /// Identifier text for a NAME token, NFKC-normalized per PEP 3131.
    ///
    /// CPython normalizes every identifier to Normalization Form KC at
    /// parse time (`unicodeobject.c: _PyUnicode_TransformDecimalAndSpaceToASCII`
    /// → `compile.c`/`tokenizer` actually use `PyUnicode_FromString` +
    /// `unicodedata.normalize('NFKC', …)`), so `µ` (U+00B5) and `μ`
    /// (U+03BC) bind the same name, and the mathematical-alphabet
    /// `𝔘𝔫𝔦𝔠𝔬𝔡𝔢` folds to plain `Unicode`. ASCII identifiers — the
    /// overwhelmingly common case — are already in NFKC, so we return the
    /// borrowed slice without touching the normalizer.
    fn ident(&self, span: Span) -> String {
        let raw = self.lexeme(span);
        if raw.is_ascii() {
            raw.to_owned()
        } else {
            use unicode_normalization::UnicodeNormalization;
            raw.nfkc().collect()
        }
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

    /// Span of the most recently consumed token. Used to extend a
    /// statement's span to its last token (CPython statement positions
    /// cover the whole statement, not just the keyword).
    fn prev_token_span(&self) -> weavepy_lexer::Span {
        self.tokens[self.pos.saturating_sub(1)].span
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

    /// Expect the `:` between a dict key and value. A `=` here gets
    /// CPython's "cannot assign to X here. Maybe you meant '==' instead
    /// of '='?", anchored at the key.
    fn expect_dict_colon(&mut self, key: &Expr) -> Result<(), ParseError> {
        if self.check(&TokenKind::Equal) {
            return Err(ParseError::Unexpected {
                span: key.span,
                message: format!(
                    "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                    crate::ast::expr_name(key)
                ),
            });
        }
        self.expect(&TokenKind::Colon, "`:`")?;
        Ok(())
    }

    /// Like [`Parser::expect`], but when the failure is two adjacent
    /// expressions (e.g. `[a b]`), report CPython's
    /// "invalid syntax. Perhaps you forgot a comma?" spanning from the
    /// previous element through the stray expression.
    fn expect_or_forgot_comma(
        &mut self,
        k: &TokenKind,
        what: &str,
        items: &[Expr],
    ) -> Result<Token, ParseError> {
        if !self.check(k) && self.at_expression_start() {
            if let Some(prev) = items.last() {
                let start = prev.span;
                // Parse (and discard) the stray expression to find its
                // extent; any nested parse error wins.
                let stray = self.parse_ternary()?;
                return Err(ParseError::Unexpected {
                    span: start.merge(stray.span),
                    message: "invalid syntax. Perhaps you forgot a comma?".to_owned(),
                });
            }
        }
        self.expect(k, what)
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
        // An INDENT token where a statement should start means the line
        // is indented deeper than its block — CPython's tokenizer
        // raises IndentationError("unexpected indent").
        if matches!(self.peek(), TokenKind::Indent) {
            return Err(ParseError::Indentation {
                span: self.peek_token().span,
                message: "unexpected indent".to_owned(),
            });
        }
        // Decorators apply to the next `def` or `class`.
        if matches!(self.peek(), TokenKind::At) {
            let decorators = self.parse_decorators()?;
            self.skip_trivia();
            return match self.peek() {
                TokenKind::Keyword(Keyword::Def) => self.parse_function_def(decorators),
                TokenKind::Keyword(Keyword::Class) => self.parse_class_def(decorators),
                TokenKind::Keyword(Keyword::Async) => self.parse_async_stmt(decorators),
                other => Err(ParseError::Unexpected {
                    span: self.peek_token().span,
                    message: format!(
                        "expected `def`, `async def`, or `class` after decorator, got {other:?}"
                    ),
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
                Keyword::Assert => self.parse_assert(),
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
                Keyword::Async => self.parse_async_stmt(Vec::new()),
                // Bare `await ...` at statement level falls through to
                // the expression-statement path; the unary parser
                // handles the `await` keyword as a prefix operator.
                Keyword::Await => self.parse_simple_statement(),
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
            // PEP 695 — `type Alias = T` soft keyword. Disambiguate
            // by requiring `type NAME = ...` shape; otherwise treat
            // `type` as an ordinary identifier (e.g. `type(x)` and
            // `type Name: ann = v` annotations).
            TokenKind::Name if self.lexeme(self.peek_token().span) == "type" => {
                if self.looks_like_type_alias_stmt() {
                    self.parse_type_alias_stmt()
                } else {
                    self.parse_simple_statement()
                }
            }
            _ => self.parse_simple_statement(),
        }
    }

    /// PEP 695 — `type X[T] = Y` lookahead.
    fn looks_like_type_alias_stmt(&self) -> bool {
        let mut i = self.pos + 1;
        // Must be followed by a name (the alias target).
        if !matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Name)) {
            return false;
        }
        i += 1;
        // Optional `[ ... ]` type-parameter list — scan to its
        // matching close-bracket at depth 0.
        if matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::LSqb)) {
            let mut depth = 1i32;
            i += 1;
            while let Some(tok) = self.tokens.get(i) {
                match &tok.kind {
                    TokenKind::LSqb => depth += 1,
                    TokenKind::RSqb => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    TokenKind::Newline | TokenKind::Endmarker => return false,
                    _ => {}
                }
                i += 1;
            }
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Equal))
    }

    /// Compile a PEP 695 type-alias statement.
    ///
    /// `type Foo[T, U] = body` desugars to:
    ///
    /// ```python
    /// Foo = (lambda T, U: body)(TypeVar('T'), TypeVar('U'))
    /// ```
    ///
    /// so the type parameters resolve as `TypeVar` instances in the
    /// alias body without leaking into the enclosing scope. The
    /// bare form `type Foo = body` lowers to plain `Foo = body`.
    fn parse_type_alias_stmt(&mut self) -> Result<Stmt, ParseError> {
        let type_tok = self.bump(); // `type`
        let name_tok = self.expect(&TokenKind::Name, "type alias name")?;
        let name = self.ident(name_tok.span);
        let type_params = self.collect_pep695_type_params()?;
        self.expect(&TokenKind::Equal, "`=`")?;
        let value = self.parse_expression_list(true)?;
        self.consume_stmt_end()?;
        let span = type_tok.span.merge(value.span);
        let target = Expr {
            kind: ExprKind::Name(name),
            span: name_tok.span,
        };
        let rhs = if type_params.is_empty() {
            value
        } else {
            wrap_in_type_param_lambda(value, &type_params, name_tok.span)
        };
        Ok(Stmt {
            kind: StmtKind::Assign {
                targets: vec![target],
                value: rhs,
            },
            span,
        })
    }

    /// Like [`Self::skip_pep695_type_params`] but returns the
    /// captured parameter names.
    fn collect_pep695_type_params(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek(), TokenKind::LSqb) {
            return Ok(Vec::new());
        }
        self.bump(); // `[`
        let mut names = Vec::new();
        loop {
            self.skip_trivia();
            // Allow `*Ts` and `**P` prefixes — discard the prefix.
            while matches!(self.peek(), TokenKind::Star | TokenKind::DoubleStar) {
                self.bump();
            }
            if matches!(self.peek(), TokenKind::RSqb) {
                break;
            }
            let name_tok = self.expect(&TokenKind::Name, "type parameter name")?;
            names.push(self.ident(name_tok.span));
            // Skip optional `: bound` and `= default`.
            if matches!(self.peek(), TokenKind::Colon) {
                self.bump();
                let _ = self.parse_expression(false)?;
            }
            if matches!(self.peek(), TokenKind::Equal) {
                self.bump();
                let _ = self.parse_expression(false)?;
            }
            if matches!(self.peek(), TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RSqb, "`]`")?;
        Ok(names)
    }

    /// PEP 695 — `[T, *Ts, **P]` after `def`/`class`/`type` names.
    /// We swallow the entire bracket-delimited list. This keeps
    /// the parser permissive for any 3.12+ syntax surface; the
    /// names are not actually bound in the function/class scope.
    fn skip_pep695_type_params(&mut self) -> Result<(), ParseError> {
        if !matches!(self.peek(), TokenKind::LSqb) {
            return Ok(());
        }
        self.bump(); // `[`
        let mut depth = 1i32;
        while let Some(tok) = self.tokens.get(self.pos) {
            match &tok.kind {
                TokenKind::LSqb => depth += 1,
                TokenKind::RSqb => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return Ok(());
                    }
                }
                TokenKind::Endmarker => break,
                _ => {}
            }
            self.bump();
        }
        Err(ParseError::Unexpected {
            span: self.peek_token().span,
            message: "unterminated type-parameter list".to_owned(),
        })
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
                    // A missing INDENT is still a match statement when
                    // `case` follows — parse_match then reports CPython's
                    // "expected an indented block after 'match' statement".
                    if matches!(self.tokens.get(j).map(|t| &t.kind), Some(TokenKind::Indent)) {
                        j += 1;
                    }
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
            // Leftover tokens after an otherwise-complete statement are
            // CPython's catch-all "invalid syntax" (e.g. `1 2`, or a bad
            // string prefix like `fu''` which tokenises as NAME + STRING).
            _ => Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: "invalid syntax".to_owned(),
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
            let end = self.prev_token_span();
            self.consume_stmt_end()?;
            return Ok(Stmt {
                kind: StmtKind::AugAssign {
                    target: first,
                    op,
                    value,
                },
                span: start_span.merge(end),
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
            let end = self.prev_token_span();
            self.consume_stmt_end()?;
            return Ok(Stmt {
                kind: StmtKind::AnnAssign {
                    target: first,
                    annotation,
                    value,
                },
                span: start_span.merge(end),
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
                    let end = self.prev_token_span();
                    self.consume_stmt_end()?;
                    return Ok(Stmt {
                        kind: StmtKind::Assign {
                            targets,
                            value: next,
                        },
                        span: start_span.merge(end),
                    });
                }
            }
            // Shouldn't reach here: we only exit the loop after the
            // final RHS has been consumed by the branch above.
            unreachable!("assignment loop fell through");
        }

        // Python-2-style `print foo` / `exec foo`: a bare `print`/`exec`
        // name directly followed by another expression. CPython special-
        // cases the diagnostic (`invalid_legacy_expression`).
        if self.at_expression_start() {
            if let ExprKind::Name(n) = &first.kind {
                if n == "print" || n == "exec" {
                    let stray = self.parse_expression_list(true)?;
                    return Err(ParseError::Unexpected {
                        span: first.span.merge(stray.span),
                        message: format!(
                            "Missing parentheses in call to '{n}'. Did you mean {n}(...)?"
                        ),
                    });
                }
            }
        }

        // Plain expression statement.
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Expr(first),
            span: start_span.merge(end),
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
        let name = self.ident(name_tok.span);
        // PEP 695: optional `[T, *Ts, **P]` type-parameter list. The
        // captured names desugar into `TypeVar` bindings around the def
        // (see `desugar_pep695_def`) so annotations referencing them
        // resolve and `f.__type_params__` is populated.
        let type_params = self.collect_pep695_type_params()?;
        self.expect(&TokenKind::LPar, "`(`")?;
        let args = self.parse_function_arguments()?;
        self.expect(&TokenKind::RPar, "`)`")?;
        let returns = if self.eat(&TokenKind::RArrow) {
            Some(self.parse_expression(false)?)
        } else {
            None
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block("function definition", def_tok.span)?;
        let span_end = body.last().map_or(def_tok.span, |s| s.span);
        let span = def_tok.span.merge(span_end);
        Ok(Stmt {
            kind: StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                returns: returns.map(Box::new),
            },
            span,
        })
    }

    /// Dispatch on the construct that follows `async`: `def`, `for`,
    /// or `with`. The `async` keyword itself was already detected by
    /// [`Self::parse_statement`] (or follows a decorator chain).
    fn parse_async_stmt(&mut self, decorator_list: Vec<Expr>) -> Result<Stmt, ParseError> {
        let async_tok = self.bump(); // `async`
        match self.peek() {
            TokenKind::Keyword(Keyword::Def) => {
                let stmt = self.parse_function_def(decorator_list)?;
                match stmt.kind {
                    StmtKind::FunctionDef {
                        name,
                        args,
                        body,
                        decorator_list,
                        type_params,
                        returns,
                    } => Ok(Stmt {
                        kind: StmtKind::AsyncFunctionDef {
                            name,
                            args,
                            body,
                            decorator_list,
                            type_params,
                            returns,
                        },
                        span: async_tok.span.merge(stmt.span),
                    }),
                    _ => unreachable!("parse_function_def returns FunctionDef"),
                }
            }
            TokenKind::Keyword(Keyword::For) => {
                if !decorator_list.is_empty() {
                    return Err(ParseError::Unexpected {
                        span: async_tok.span,
                        message: "decorators only apply to `async def`, not `async for`".to_owned(),
                    });
                }
                let stmt = self.parse_for()?;
                match stmt.kind {
                    StmtKind::For {
                        target,
                        iter,
                        body,
                        orelse,
                    } => Ok(Stmt {
                        kind: StmtKind::AsyncFor {
                            target,
                            iter,
                            body,
                            orelse,
                        },
                        span: async_tok.span.merge(stmt.span),
                    }),
                    _ => unreachable!("parse_for returns For"),
                }
            }
            TokenKind::Keyword(Keyword::With) => {
                if !decorator_list.is_empty() {
                    return Err(ParseError::Unexpected {
                        span: async_tok.span,
                        message: "decorators only apply to `async def`, not `async with`"
                            .to_owned(),
                    });
                }
                let stmt = self.parse_with()?;
                match stmt.kind {
                    StmtKind::With { items, body } => Ok(Stmt {
                        kind: StmtKind::AsyncWith { items, body },
                        span: async_tok.span.merge(stmt.span),
                    }),
                    _ => unreachable!("parse_with returns With"),
                }
            }
            other => Err(ParseError::Unexpected {
                span: self.peek_token().span,
                message: format!("expected `def`, `for`, or `with` after `async`, got {other:?}"),
            }),
        }
    }

    fn parse_class_def(&mut self, decorator_list: Vec<Expr>) -> Result<Stmt, ParseError> {
        let class_tok = self.bump(); // `class`
        let name_tok = self.expect(&TokenKind::Name, "class name")?;
        let name = self.ident(name_tok.span);
        // PEP 695: optional `[T, *Ts, **P]` type-parameter list — same
        // desugar as the function form (TypeVar bindings around the def).
        let type_params = self.collect_pep695_type_params()?;
        let (bases, keywords) = if self.eat(&TokenKind::LPar) {
            let (a, kw) = self.parse_call_args()?;
            self.expect(&TokenKind::RPar, "`)`")?;
            (a, kw)
        } else {
            (Vec::new(), Vec::new())
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block("class definition", class_tok.span)?;
        let span_end = body.last().map_or(class_tok.span, |s| s.span);
        let span = class_tok.span.merge(span_end);
        Ok(Stmt {
            kind: StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            },
            span,
        })
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        let try_tok = self.bump(); // `try`
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block("'try' statement", try_tok.span)?;
        self.skip_trivia_and_newlines();
        let mut handlers = Vec::new();
        let mut orelse: Vec<Stmt> = Vec::new();
        let mut finalbody: Vec<Stmt> = Vec::new();
        let mut saw_star = false;
        let mut saw_plain = false;
        while self.at_keyword(Keyword::Except) {
            let exc_tok = self.bump(); // `except`
            let is_star = matches!(self.peek(), TokenKind::Star);
            if is_star {
                self.bump();
                saw_star = true;
            } else {
                saw_plain = true;
            }
            if saw_star && saw_plain {
                return Err(ParseError::Unexpected {
                    span: exc_tok.span,
                    message: "cannot have both 'except' and 'except*' on the same try".to_owned(),
                });
            }
            let (type_, name) = if self.check(&TokenKind::Colon) {
                if is_star {
                    // CPython anchors this at the token where the
                    // exception type should have appeared (the `:`).
                    return Err(ParseError::Unexpected {
                        span: self.peek_token().span,
                        message: "expected one or more exception types".to_owned(),
                    });
                }
                (None, None)
            } else {
                let t = self.parse_expression(false)?;
                let n = if self.at_keyword(Keyword::As) {
                    self.bump();
                    let nt = self.expect(&TokenKind::Name, "name after `as`")?;
                    Some(self.ident(nt.span))
                } else {
                    None
                };
                (Some(t), n)
            };
            self.expect(&TokenKind::Colon, "`:`")?;
            let handler_body = self.parse_block("'except' statement", exc_tok.span)?;
            let span_end = handler_body.last().map_or(exc_tok.span, |s| s.span);
            handlers.push(ExceptHandler {
                type_,
                name,
                body: handler_body,
                span: exc_tok.span.merge(span_end),
                is_star,
            });
            self.skip_trivia_and_newlines();
        }
        if self.at_keyword(Keyword::Else) {
            let else_tok = self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            orelse = self.parse_block("'else' statement", else_tok.span)?;
            self.skip_trivia_and_newlines();
        }
        if self.at_keyword(Keyword::Finally) {
            let fin_tok = self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            finalbody = self.parse_block("'finally' statement", fin_tok.span)?;
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
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Raise { exc, cause },
            span: kw.span.merge(end),
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
        let body = self.parse_block("'with' statement", kw.span)?;
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
        // A bare `*` separator (no `*args` name) requires at least one
        // keyword-only argument to follow it; CPython rejects `def f(p, *)`
        // and `def f(p, *, **kw)` with "named arguments must follow bare *".
        let mut bare_star_span: Option<Span> = None;
        loop {
            if self.check(&TokenKind::RPar) || self.check(&TokenKind::Colon) {
                break;
            }
            // `*args` or bare `*` separator.
            if self.check(&TokenKind::Star) {
                let star_tok = self.bump();
                if matches!(self.peek(), TokenKind::Name) {
                    let n = self.bump();
                    args.vararg = Some(Arg {
                        name: self.ident(n.span),
                        annotation: self.try_arg_annotation(allow_annotation)?,
                        span: n.span,
                    });
                } else {
                    bare_star_span = Some(star_tok.span);
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
                    name: self.ident(n.span),
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
            let name = self.ident(n.span);
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
        // A bare `*` must be followed by at least one keyword-only
        // argument (CPython: "named arguments must follow bare *").
        if let Some(span) = bare_star_span {
            if args.kwonlyargs.is_empty() {
                return Err(ParseError::Unexpected {
                    span,
                    message: "named arguments must follow bare *".to_owned(),
                });
            }
        }
        // No parameter name may repeat across any section
        // (positional-only, positional-or-keyword, `*args`, keyword-only,
        // `**kwargs`) — CPython raises "duplicate argument '<n>' in
        // function definition".
        let mut seen: Vec<&str> = Vec::new();
        let dup_span = |span: Span, name: &str| ParseError::Unexpected {
            span,
            message: format!("duplicate argument '{name}' in function definition"),
        };
        for a in args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.vararg.iter())
            .chain(args.kwonlyargs.iter())
            .chain(args.kwarg.iter())
        {
            if seen.contains(&a.name.as_str()) {
                return Err(dup_span(a.span, &a.name));
            }
            seen.push(a.name.as_str());
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
        let if_tok = self.bump(); // `if` (or `elif` — see recursion below)
        let if_what = if matches!(if_tok.kind, TokenKind::Keyword(Keyword::Elif)) {
            "'elif' statement"
        } else {
            "'if' statement"
        };
        let test = self.parse_expression(false)?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.parse_block(if_what, if_tok.span)?;
        let orelse = if self.at_keyword(Keyword::Elif) {
            // Recurse: elif → nested `if`.
            let nested = self.parse_if()?;
            vec![nested]
        } else if self.at_keyword(Keyword::Else) {
            let else_tok = self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block("'else' statement", else_tok.span)?
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
        let body = self.parse_block("'while' statement", kw.span)?;
        let orelse = if self.at_keyword(Keyword::Else) {
            let else_tok = self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block("'else' statement", else_tok.span)?
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
        let body = self.parse_block("'for' statement", kw.span)?;
        let orelse = if self.at_keyword(Keyword::Else) {
            let else_tok = self.bump();
            self.expect(&TokenKind::Colon, "`:`")?;
            self.parse_block("'else' statement", else_tok.span)?
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
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
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
                Some(self.ident(n.span))
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
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Import(names),
            span: kw.span.merge(end),
        })
    }

    fn parse_dotted_name(&mut self) -> Result<String, ParseError> {
        let first = self.expect(&TokenKind::Name, "module name")?;
        let mut out = self.ident(first.span);
        while self.eat(&TokenKind::Dot) {
            let n = self.expect(&TokenKind::Name, "name after `.`")?;
            out.push('.');
            out.push_str(&self.ident(n.span));
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
                let name = self.ident(n.span);
                let asname = if self.at_keyword(Keyword::As) {
                    self.bump();
                    let n2 = self.expect(&TokenKind::Name, "name after `as`")?;
                    Some(self.ident(n2.span))
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
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::ImportFrom {
                module,
                names,
                level,
            },
            span: kw.span.merge(end),
        })
    }

    fn parse_global(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let names = self.parse_name_list()?;
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Global(names),
            span: kw.span.merge(end),
        })
    }

    fn parse_nonlocal(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let names = self.parse_name_list()?;
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Nonlocal(names),
            span: kw.span.merge(end),
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
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Delete(targets),
            span: kw.span.merge(end),
        })
    }

    /// `assert <test> [, <msg>]`
    ///
    /// The grammar is `'assert' test [',' test]` — the message is an
    /// arbitrary expression (not a `,`-separated tuple shorthand). We
    /// accept and store both for the compiler to lower.
    fn parse_assert(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.bump();
        let test = self.parse_ternary()?;
        let msg = if self.eat(&TokenKind::Comma) {
            Some(self.parse_ternary()?)
        } else {
            None
        };
        let end = self.prev_token_span();
        self.consume_stmt_end()?;
        Ok(Stmt {
            kind: StmtKind::Assert { test, msg },
            span: kw.span.merge(end),
        })
    }

    fn parse_name_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut names = Vec::new();
        loop {
            let n = self.expect(&TokenKind::Name, "name")?;
            names.push(self.ident(n.span));
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
        if !self.check(&TokenKind::Indent) {
            return Err(ParseError::Indentation {
                span: self.peek_token().span,
                message: format!(
                    "expected an indented block after 'match' statement on line {}",
                    self.line_of(kw.span)
                ),
            });
        }
        self.bump();

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
        let body = self.parse_block("'case' statement", case_tok.span)?;
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
            let name = self.ident(n.span);
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
                    let s = self.ident(tok.span);
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
                Constant::BigInt(s) => {
                    Constant::BigInt(if let Some(stripped) = s.strip_prefix('-') {
                        stripped.to_owned()
                    } else {
                        format!("-{s}")
                    })
                }
                Constant::Complex(real, imag) => Constant::Complex(-real, -imag),
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
        let first_name = self.ident(first.span);
        // Dotted: value pattern.
        if self.check(&TokenKind::Dot) {
            let mut expr = Expr {
                kind: ExprKind::Name(first_name),
                span: first.span,
            };
            while self.eat(&TokenKind::Dot) {
                let n = self.expect(&TokenKind::Name, "attribute name in pattern")?;
                let attr = self.ident(n.span);
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
                let name = self.ident(n.span);
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
                let name = self.ident(n.span);
                rest = Some(if name == "_" { None } else { Some(name) });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                // PEP 634: `**rest` must be the last item in a mapping
                // pattern. CPython reports a plain "invalid syntax"
                // anchored at the trailing item.
                if !self.check(&TokenKind::RBrace) {
                    return Err(ParseError::Unexpected {
                        span: self.peek_token().span,
                        message: "invalid syntax".to_owned(),
                    });
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
            kind: ExprKind::Name(self.ident(n.span)),
            span: n.span,
        };
        while self.eat(&TokenKind::Dot) {
            let attr_tok = self.expect(&TokenKind::Name, "attribute name in key")?;
            let attr = self.ident(attr_tok.span);
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

    /// 1-based source line containing the start of `span`.
    fn line_of(&self, span: weavepy_lexer::Span) -> u32 {
        let start = (span.start.0 as usize).min(self.source.len());
        self.source[..start].bytes().filter(|&b| b == b'\n').count() as u32 + 1
    }

    /// Block suite after `:`. `after` names the introducing construct
    /// for CPython's IndentationError message ("'if' statement",
    /// "function definition", …); `intro_span` is the keyword's span,
    /// whose line number the message cites.
    fn parse_block(
        &mut self,
        after: &str,
        intro_span: weavepy_lexer::Span,
    ) -> Result<Vec<Stmt>, ParseError> {
        // After the `:`, either a single inline statement or a
        // NEWLINE INDENT block DEDENT.
        if matches!(self.peek(), TokenKind::Newline) {
            self.bump();
            self.skip_trivia_and_newlines();
            if !self.check(&TokenKind::Indent) {
                return Err(ParseError::Indentation {
                    span: self.peek_token().span,
                    message: format!(
                        "expected an indented block after {after} on line {}",
                        self.line_of(intro_span)
                    ),
                });
            }
            self.bump();
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
            // Inline suite after `:` — Python's `simple_stmt`:
            //   small_stmt (';' small_stmt)* [';'] NEWLINE
            // e.g. `if x: y = 1`, `class A: pass`, and crucially the
            // multi-statement form `def f(): a = 1; return a`. We used to
            // parse only the first statement, leaving `; return a` to be
            // re-parsed by the *enclosing* scope — which then rejected the
            // `return` as "outside function". Each `parse_statement`
            // consumes its own terminator (`;` or NEWLINE via
            // `consume_stmt_end`), so we keep going while that terminator
            // was a `;` and another small statement follows on the line.
            let mut body = Vec::new();
            loop {
                body.push(self.parse_statement()?);
                let ended_with_semi = matches!(
                    self.tokens.get(self.pos.wrapping_sub(1)).map(|t| &t.kind),
                    Some(TokenKind::Semi)
                );
                if !ended_with_semi {
                    break;
                }
                // A trailing `;` right before the line break (`a = 1;`)
                // ends the suite; consume the closing NEWLINE so the
                // caller resumes from a clean statement boundary.
                match self.peek() {
                    TokenKind::Newline => {
                        self.bump();
                        break;
                    }
                    TokenKind::Endmarker | TokenKind::Dedent => break,
                    _ => {}
                }
            }
            Ok(body)
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
    ///
    /// Accepts `*x` as a sub-element: this is what makes both
    /// PEP 3132 assignment targets (`a, *b, c = xs`) and the
    /// general iterable-unpacking case in collection literals fall
    /// out of a single parse.
    fn parse_expression_list(&mut self, _allow_trailing_comma: bool) -> Result<Expr, ParseError> {
        let first = self.parse_ternary_or_starred()?;
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
            items.push(self.parse_ternary_or_starred()?);
        }
        let end_span = items.last().expect("nonempty").span;
        Ok(Expr {
            kind: ExprKind::Tuple(items),
            span: start_span.merge(end_span),
        })
    }

    /// `*expr` or a regular ternary. Used wherever a comma-separated
    /// element may legitimately be a starred form (assignment
    /// targets, tuple/list/set literals, function call arguments).
    fn parse_ternary_or_starred(&mut self) -> Result<Expr, ParseError> {
        if let TokenKind::Star = self.peek() {
            let star_tok = self.peek_token().clone();
            self.bump();
            let inner = self.parse_ternary()?;
            // Token-range span (CPython's EXTRA): a parenthesized
            // operand's node span excludes its parens, so merge with
            // the last *consumed token* instead of the child span.
            let span = star_tok.span.merge(self.prev_token_span());
            return Ok(Expr {
                kind: ExprKind::Starred(Box::new(inner)),
                span,
            });
        }
        self.parse_ternary()
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
        // PEP 572 walrus `NAME := expr`. The named-expression form
        // must syntactically be exactly a name followed by `:=`; the
        // compiler enforces the rest of the PEP's restrictions
        // (no assignment expressions at module scope rules).
        if matches!(self.peek(), TokenKind::Name) {
            if let Some(next) = self.tokens.get(self.pos + 1) {
                if matches!(next.kind, TokenKind::ColonEqual) {
                    let name_tok = self.peek_token().clone();
                    let name = self.ident(name_tok.span);
                    self.bump(); // name
                    self.bump(); // :=
                    let value = self.parse_ternary()?;
                    let span = name_tok.span.merge(self.prev_token_span());
                    return Ok(Expr {
                        kind: ExprKind::NamedExpr {
                            target: Box::new(Expr {
                                kind: ExprKind::Name(name),
                                span: name_tok.span,
                            }),
                            value: Box::new(value),
                        },
                        span,
                    });
                }
            }
        }
        let start = self.peek_token().span;
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
            let span = start.merge(self.prev_token_span());
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
            let span = kw.span.merge(self.prev_token_span());
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
        let span = kw.span.merge(self.prev_token_span());
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
        let span = kw.span.merge(self.prev_token_span());
        Ok(Expr {
            kind: ExprKind::Lambda {
                args,
                body: Box::new(body),
            },
            span,
        })
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let start = self.peek_token().span;
        let first = self.parse_and()?;
        if !self.at_keyword(Keyword::Or) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.at_keyword(Keyword::Or) {
            self.bump();
            values.push(self.parse_and()?);
        }
        let span = start.merge(self.prev_token_span());
        Ok(Expr {
            kind: ExprKind::BoolOp {
                op: BoolOp::Or,
                values,
            },
            span,
        })
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let start = self.peek_token().span;
        let first = self.parse_not()?;
        if !self.at_keyword(Keyword::And) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.at_keyword(Keyword::And) {
            self.bump();
            values.push(self.parse_not()?);
        }
        let span = start.merge(self.prev_token_span());
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
            let span = kw.span.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
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
        let span = start.merge(self.prev_token_span());
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
        // All the binop ladders use token-range spans (`start` is the
        // first token of the rule, the end is the last consumed token)
        // so a parenthesized operand — whose node span excludes the
        // parens, as in CPython — still yields `(a) + b` spanning from
        // the `(`. PEP-657 caret anchors depend on this.
        let start = self.peek_token().span;
        let mut left = self.parse_bit_xor()?;
        while self.check(&TokenKind::Vbar) {
            self.bump();
            let right = self.parse_bit_xor()?;
            let span = start.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
        let mut left = self.parse_bit_and()?;
        while self.check(&TokenKind::Caret) {
            self.bump();
            let right = self.parse_bit_and()?;
            let span = start.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
        let mut left = self.parse_shift()?;
        while self.check(&TokenKind::Amper) {
            self.bump();
            let right = self.parse_shift()?;
            let span = start.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
        let mut left = self.parse_addsub()?;
        loop {
            let op = match self.peek() {
                TokenKind::LeftShift => BinOp::LShift,
                TokenKind::RightShift => BinOp::RShift,
                _ => break,
            };
            self.bump();
            let right = self.parse_addsub()?;
            let span = start.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
        let mut left = self.parse_muldiv()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let right = self.parse_muldiv()?;
            let span = start.merge(self.prev_token_span());
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
        let start = self.peek_token().span;
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
            let span = start.merge(self.prev_token_span());
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
        // `await expr` (PEP 492 / RFC 0016). `await` sits at the
        // unary level — `await x + y` parses as `(await x) + y`,
        // matching CPython.
        if self.at_keyword(Keyword::Await) {
            let kw = self.bump();
            // CPython grammar: `await_primary: AWAIT primary` — a
            // directly chained `await await x` is invalid syntax
            // (`await (await x)` is fine: the parens make a primary).
            if self.at_keyword(Keyword::Await) {
                return Err(ParseError::Unexpected {
                    span: kw.span,
                    message: "invalid syntax".to_owned(),
                });
            }
            let operand = self.parse_unary()?;
            let span = kw.span.merge(self.prev_token_span());
            return Ok(Expr {
                kind: ExprKind::Await(Box::new(operand)),
                span,
            });
        }
        let op = match self.peek() {
            TokenKind::Plus => UnaryOp::UAdd,
            TokenKind::Minus => UnaryOp::USub,
            TokenKind::Tilde => UnaryOp::Invert,
            _ => return self.parse_power(),
        };
        let kw = self.bump();
        let operand = self.parse_unary()?;
        let span = kw.span.merge(self.prev_token_span());
        Ok(Expr {
            kind: ExprKind::UnaryOp {
                op,
                operand: Box::new(operand),
            },
            span,
        })
    }

    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let start = self.peek_token().span;
        let base = self.parse_trailer_chain()?;
        if !self.check(&TokenKind::DoubleStar) {
            return Ok(base);
        }
        self.bump();
        // `**` is right-associative, and binds tighter than unary on
        // the right side. Python's grammar: `power: await? primary ('**' factor)?`
        // where `factor` includes unary.
        let exponent = self.parse_unary()?;
        let span = start.merge(self.prev_token_span());
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
        // Token-range spans: `( a ).m()[0]` chains span from the `(`
        // even though the parenthesized base node itself starts at `a`.
        let start = self.peek_token().span;
        let mut base = self.parse_atom()?;
        loop {
            match self.peek() {
                TokenKind::LPar => {
                    self.bump();
                    let (args, keywords) = self.parse_call_args()?;
                    let rp = self.expect(&TokenKind::RPar, "`)`")?;
                    let span = start.merge(rp.span);
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
                    let span = start.merge(rb.span);
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
                    let attr = self.ident(n.span);
                    let span = start.merge(n.span);
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
        // Track keyword state so we can reject a plain positional argument
        // that follows a keyword (CPython: "positional argument follows
        // keyword argument") and a repeated keyword name (CPython:
        // "keyword argument repeated: <name>").
        let mut seen_keyword = false;
        let mut kw_names: Vec<String> = Vec::new();
        // Span of the most recent argument — CPython's "Perhaps you
        // forgot a comma?" underlines the *previous* element when two
        // expressions abut.
        let mut last_arg_span: Option<weavepy_lexer::Span> = None;
        loop {
            let arg_start = self.peek_token().span;
            if self.eat(&TokenKind::DoubleStar) {
                let val = self.parse_ternary()?;
                seen_keyword = true;
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
                    let name = self.ident(nt.span);
                    if kw_names.contains(&name) {
                        return Err(ParseError::Unexpected {
                            span: nt.span,
                            message: format!("keyword argument repeated: {name}"),
                        });
                    }
                    self.bump(); // `=`
                    let val = self.parse_ternary()?;
                    seen_keyword = true;
                    kw_names.push(name.clone());
                    keywords.push(KwArg {
                        arg: Some(name),
                        value: val,
                    });
                } else {
                    if seen_keyword {
                        return Err(ParseError::Unexpected {
                            span: self.peek_token().span,
                            message: "positional argument follows keyword argument".to_owned(),
                        });
                    }
                    let e = self.parse_ternary()?;
                    // `f(1=2)` — a non-name expression followed by `=`.
                    if self.check(&TokenKind::Equal) {
                        return Err(ParseError::Unexpected {
                            span: e.span,
                            message: "expression cannot contain assignment, perhaps you meant \
                                      \"==\"?"
                                .to_owned(),
                        });
                    }
                    // Generator expression as single argument: `f(x for x in xs)`.
                    if self.at_keyword(Keyword::For) || self.at_keyword(Keyword::Async) {
                        let elt = e;
                        let generators = self.parse_comp_for()?;
                        // CPython's genexp node (and the
                        // must-be-parenthesized error) spans the whole
                        // `elt for … in …`, not just the element.
                        let span = elt.span.merge(self.prev_token_span());
                        // A bare genexp must be the *only* argument.
                        if !(args.is_empty() && keywords.is_empty()) {
                            return Err(ParseError::Unexpected {
                                span,
                                message: "Generator expression must be parenthesized"
                                    .to_owned(),
                            });
                        }
                        args.push(Expr {
                            kind: ExprKind::GeneratorExp {
                                elt: Box::new(elt),
                                generators,
                            },
                            span,
                        });
                        if self.check(&TokenKind::Comma) {
                            return Err(ParseError::Unexpected {
                                span,
                                message: "Generator expression must be parenthesized"
                                    .to_owned(),
                            });
                        }
                    } else {
                        args.push(e);
                    }
                }
            }
            last_arg_span = Some(arg_start.merge(self.prev_token_span()));
            if !self.eat(&TokenKind::Comma) {
                // Two adjacent expressions with no comma — CPython's
                // `invalid_expression` rule anchors the error at the
                // *previous* element and suggests the missing comma.
                if !self.check(&TokenKind::RPar) && self.at_expression_start() {
                    if let Some(prev) = last_arg_span {
                        return Err(ParseError::Unexpected {
                            span: prev,
                            message: "invalid syntax. Perhaps you forgot a comma?".to_owned(),
                        });
                    }
                }
                break;
            }
            if self.check(&TokenKind::RPar) {
                break;
            }
        }
        Ok((args, keywords))
    }

    /// Could the current token begin an expression? Drives the
    /// "Perhaps you forgot a comma?" heuristic.
    fn at_expression_start(&self) -> bool {
        match self.peek() {
            TokenKind::Name
            | TokenKind::Number
            | TokenKind::String
            | TokenKind::LPar
            | TokenKind::LSqb
            | TokenKind::LBrace
            | TokenKind::Minus
            | TokenKind::Plus
            | TokenKind::Tilde
            | TokenKind::Ellipsis => true,
            TokenKind::Keyword(k) => matches!(
                k,
                Keyword::Lambda
                    | Keyword::Not
                    | Keyword::Await
                    | Keyword::None
                    | Keyword::True
                    | Keyword::False
            ),
            _ => false,
        }
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
                    kind: ExprKind::Name(self.ident(tok.span)),
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
        if self.check(&TokenKind::RPar) {
            // Empty tuple — spans both parens.
            let rp = self.bump();
            return Ok(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: lp.span.merge(rp.span),
            });
        }
        let first = self.parse_ternary_or_starred()?;
        let first_starred = matches!(first.kind, ExprKind::Starred(_));
        // Generator expression?
        if !first_starred && (self.at_keyword(Keyword::For) || self.at_keyword(Keyword::Async)) {
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
                items.push(self.parse_ternary_or_starred()?);
            }
            let rp = self.expect_or_forgot_comma(&TokenKind::RPar, "`)`", &items)?;
            return Ok(Expr {
                kind: ExprKind::Tuple(items),
                span: lp.span.merge(rp.span),
            });
        }
        // A bare `(*a)` with no trailing comma is a syntax error in
        // CPython — starred expressions are only legal inside a tuple/
        // call/assignment context, never as a lone parenthesized value.
        if first_starred {
            return Err(ParseError::Unexpected {
                span: first.span,
                message: "can't use starred expression here".to_owned(),
            });
        }
        // Plain parenthesized expression: no wrapper node, and — exactly
        // like CPython's grammar actions — the node keeps the *inner*
        // span, excluding the parentheses. `traceback`'s caret-anchor
        // probe (`ast.parse(f"(\n{segment}\n)")`) depends on this:
        // positions must point into `segment`, not at the added parens.
        self.expect_or_forgot_comma(&TokenKind::RPar, "`)`", std::slice::from_ref(&first))?;
        Ok(first)
    }

    fn parse_list_or_listcomp(&mut self) -> Result<Expr, ParseError> {
        let lb = self.bump();
        if self.eat(&TokenKind::RSqb) {
            return Ok(Expr {
                kind: ExprKind::List(Vec::new()),
                span: lb.span,
            });
        }
        let first = self.parse_ternary_or_starred()?;
        let first_starred = matches!(first.kind, ExprKind::Starred(_));
        if self.at_keyword(Keyword::For) || self.at_keyword(Keyword::Async) {
            if first_starred {
                return Err(ParseError::Unexpected {
                    span: first.span,
                    message: "iterable unpacking cannot be used in comprehension".to_owned(),
                });
            }
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
            items.push(self.parse_ternary_or_starred()?);
        }
        let rb = self.expect_or_forgot_comma(&TokenKind::RSqb, "`]`", &items)?;
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
                    self.expect_dict_colon(&k)?;
                    let v = self.parse_ternary()?;
                    keys.push(Some(k));
                    values.push(v);
                }
            }
            let rb = self.expect_or_forgot_comma(&TokenKind::RBrace, "`}`", &values)?;
            return Ok(Expr {
                kind: ExprKind::Dict { keys, values },
                span: lb.span.merge(rb.span),
            });
        }
        let first = self.parse_ternary_or_starred()?;
        let first_starred = matches!(first.kind, ExprKind::Starred(_));
        if !first_starred && self.eat(&TokenKind::Colon) {
            // Dict literal (or dict comprehension).
            let v = self.parse_ternary()?;
            if self.at_keyword(Keyword::For) || self.at_keyword(Keyword::Async) {
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
                    self.expect_dict_colon(&k)?;
                    let vv = self.parse_ternary()?;
                    keys.push(Some(k));
                    values.push(vv);
                }
            }
            let rb = self.expect_or_forgot_comma(&TokenKind::RBrace, "`}`", &values)?;
            return Ok(Expr {
                kind: ExprKind::Dict { keys, values },
                span: lb.span.merge(rb.span),
            });
        }
        // Otherwise: set literal or set comp.
        if self.at_keyword(Keyword::For) || self.at_keyword(Keyword::Async) {
            if first_starred {
                return Err(ParseError::Unexpected {
                    span: first.span,
                    message: "iterable unpacking cannot be used in comprehension".to_owned(),
                });
            }
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
        // A `=` right after a would-be dict key: `{k=22}`. CPython:
        // "cannot assign to <expr> here. Maybe you meant '==' instead
        // of '='?", anchored at the key.
        if self.check(&TokenKind::Equal) {
            return Err(ParseError::Unexpected {
                span: first.span,
                message: format!(
                    "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                    crate::ast::expr_name(&first)
                ),
            });
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.check(&TokenKind::RBrace) {
                break;
            }
            items.push(self.parse_ternary_or_starred()?);
            if self.check(&TokenKind::Equal) {
                let last = items.last().expect("just pushed");
                return Err(ParseError::Unexpected {
                    span: last.span,
                    message: format!(
                        "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                        crate::ast::expr_name(last)
                    ),
                });
            }
        }
        let rb = self.expect_or_forgot_comma(&TokenKind::RBrace, "`}`", &items)?;
        Ok(Expr {
            kind: ExprKind::Set(items),
            span: lb.span.merge(rb.span),
        })
    }

    fn parse_comp_for(&mut self) -> Result<Vec<Comprehension>, ParseError> {
        let mut generators = Vec::new();
        loop {
            // PEP 530: `[x async for x in it]` — an `async` prefix on
            // a comprehension `for` clause marks the generator as
            // async-iterable. The enclosing context must be an
            // `async def`; the compiler enforces that.
            let is_async = if self.at_keyword(Keyword::Async) {
                self.bump();
                if !self.at_keyword(Keyword::For) {
                    return Err(ParseError::Unexpected {
                        span: self.peek_token().span,
                        message: "expected `for` after `async` in comprehension".to_owned(),
                    });
                }
                true
            } else if self.at_keyword(Keyword::For) {
                false
            } else {
                break;
            };
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
                is_async,
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
        let first = self.parse_target_or_star()?;
        if !self.check(&TokenKind::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at_keyword(Keyword::In) {
                break;
            }
            items.push(self.parse_target_or_star()?);
        }
        let span = items[0].span.merge(items.last().unwrap().span);
        Ok(Expr {
            kind: ExprKind::Tuple(items),
            span,
        })
    }

    /// One element of an assignment target list. Accepts both plain
    /// targets (`name`, `name.attr`, `name[i]`) and PEP 3132 starred
    /// targets (`*name`). The compiler enforces "at most one star
    /// per list" later.
    fn parse_target_or_star(&mut self) -> Result<Expr, ParseError> {
        if let TokenKind::Star = self.peek() {
            let star_tok = self.peek_token().clone();
            self.bump();
            let inner = self.parse_unary()?;
            let span = star_tok.span.merge(inner.span);
            return Ok(Expr {
                kind: ExprKind::Starred(Box::new(inner)),
                span,
            });
        }
        self.parse_unary()
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
                        message: "cannot mix bytes and nonbytes literals".to_owned(),
                    });
                }
                (_, _, true) => {
                    return Err(ParseError::Unexpected {
                        span: next_tok.span,
                        message: "cannot mix bytes and nonbytes literals".to_owned(),
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
            // Any f-string in the concatenation keeps the result a
            // `JoinedStr`, even when it folds to a single literal part
            // (CPython AST shape; not a docstring candidate).
            AccumString::Joined(parts) => Ok(Expr {
                kind: ExprKind::JoinedStr(parts),
                span,
            }),
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
        // Absolute byte offset of `body` in the source — `body` is a
        // subslice of the token's lexeme, so pointer arithmetic gives
        // the prefix+quote length. Interior parse errors are reported
        // at their real source location (PEP 701), not the f-string's.
        let body_abs = tok.span.start.0 + (body.as_ptr() as usize - lex.as_ptr() as usize) as u32;
        let parts = self.parse_fstring_body(body, raw, tok.span, body_abs)?;
        // CPython keeps even a pure-literal f-string as a `JoinedStr`
        // (`ast.parse("f'x'")` is JoinedStr([Constant])): it is *not* a
        // docstring candidate and `ast.literal_eval` rejects it. Constant
        // folding happens in the compiler, not here.
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
        body_abs: u32,
    ) -> Result<Vec<Expr>, ParseError> {
        self.parse_fstring_body_inner(body, raw, anchor, body_abs, false)
    }

    /// `spec_mode` parses a format spec, where `{{`/`}}` are *not*
    /// doubled-brace escapes: in a spec every `{` opens a nested
    /// replacement field (CPython rejects `f'{3:{{>10}'` with "expecting
    /// a valid expression after '{'").
    fn parse_fstring_body_inner(
        &self,
        body: &str,
        raw: bool,
        anchor: Span,
        body_abs: u32,
        spec_mode: bool,
    ) -> Result<Vec<Expr>, ParseError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        // Byte index in `body` where the current literal run began, for
        // the Constant part's source span (CPython anchors each literal
        // part at its own text, not at the whole string token).
        let mut lit_start = 0usize;
        let bytes = body.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if literal.is_empty() {
                lit_start = i;
            }
            // Non-raw backslash escapes are copied into the literal as a
            // unit so the decoder interprets them — and, crucially, so an
            // escaped backslash (`\\`) can't have its second byte misread
            // as the start of a new escape (e.g. `f'\\N{AMPERSAND}'` is a
            // literal `\` then the field `{AMPERSAND}`, not `\N{...}`).
            if b == b'\\' && !raw {
                // `\N{NAME}` named-character escape: the brace group is
                // the Unicode character name, not a replacement field.
                if bytes.get(i + 1) == Some(&b'N') && bytes.get(i + 2) == Some(&b'{') {
                    let mut j = i + 3;
                    while j < bytes.len() && bytes[j] != b'}' {
                        j += 1;
                    }
                    if j < bytes.len() {
                        j += 1; // include the closing `}`
                    }
                    literal.push_str(&body[i..j]);
                    i = j;
                    continue;
                }
                // Any other escape: copy the backslash, then its escaped
                // character — except `{`/`}`, which stay structural (a
                // lone `\` before a brace is a literal backslash followed
                // by a replacement field / brace escape, e.g. `\{6*7}`).
                literal.push('\\');
                i += 1;
                if let Some(&n) = bytes.get(i) {
                    if n != b'{' && n != b'}' {
                        let ch_len = utf8_char_len(n);
                        let end = (i + ch_len).min(bytes.len());
                        literal.push_str(&body[i..end]);
                        i = end;
                    }
                }
                continue;
            }
            if b == b'{' {
                if !spec_mode && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
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
                        span: Span::new(body_abs + lit_start as u32, body_abs + i as u32),
                    });
                    literal.clear();
                }
                let (field, end) = match self.scan_fstring_field(body, i + 1, anchor) {
                    Ok(ok) => ok,
                    // Unterminated field ("f-string: expecting '}'"):
                    // CPython's pegen parses the partial expression
                    // first, so an *inner* error (e.g. "Perhaps you
                    // forgot a comma?") wins over the missing brace.
                    Err(scan_err) => {
                        if matches!(&scan_err, ParseError::Unexpected { message, .. }
                            if message == "f-string: expecting '}'")
                        {
                            let partial = &body[i + 1..];
                            self.parse_fstring_field(
                                partial,
                                anchor,
                                body_abs + (i as u32) + 1,
                                raw,
                                true,
                            )?;
                        }
                        return Err(scan_err);
                    }
                };
                let parsed =
                    self.parse_fstring_field(&field, anchor, body_abs + (i as u32) + 1, raw, false)?;
                parts.push(parsed);
                i = end + 1; // skip past the closing `}`
                continue;
            }
            if b == b'}' {
                if !spec_mode && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    literal.push('}');
                    i += 2;
                    continue;
                }
                return Err(ParseError::Unexpected {
                    span: anchor,
                    message: "f-string: single '}' is not allowed".to_owned(),
                });
            }
            // Append the next UTF-8 character (one or more bytes).
            let ch_len = utf8_char_len(b);
            let end = (i + ch_len).min(bytes.len());
            literal.push_str(&body[i..end]);
            i = end;
        }
        // A trailing literal run becomes a final part; an empty body
        // yields an empty `JoinedStr` (CPython: `f''` and the empty
        // format spec `f'{x:}'` both have `values=[]`).
        if !literal.is_empty() {
            let decoded = decode_str_body(&literal, raw).map_err(|m| ParseError::Unexpected {
                span: anchor,
                message: m,
            })?;
            parts.push(Expr {
                kind: ExprKind::Constant(Constant::Str(decoded)),
                span: Span::new(body_abs + lit_start as u32, body_abs + i as u32),
            });
        }
        Ok(parts)
    }

    /// Scan from just past the opening `{` to the matching `}` at the
    /// field's top level. Returns the field text and the index of that
    /// closing `}`.
    ///
    /// Bracket nesting is tracked with an explicit stack so we can report
    /// CPython's PEP 701 diagnostics: a `)`/`]`/`}` that doesn't match the
    /// innermost opener yields "closing parenthesis 'X' does not match
    /// opening parenthesis 'Y'", a `)`/`]` with nothing open yields
    /// "f-string: unmatched ')'", and running off the end yields
    /// "f-string: expecting '}'".
    fn scan_fstring_field(
        &self,
        body: &str,
        start: usize,
        anchor: Span,
    ) -> Result<(String, usize), ParseError> {
        let bytes = body.as_bytes();
        // Openers seen *inside* the field (the field's own `{` is implicit
        // and not pushed); a top-level `}` closes the field.
        let mut stack: Vec<u8> = Vec::new();
        let mut i = start;
        // String state machine for quotes inside the field.
        let mut in_str: Option<u8> = None;
        let mut triple = false;
        // Once the top-level `:` is seen we're in the format spec, where
        // `#` is literal (e.g. `{x:#06x}`); before it, in the expression
        // part, `#` starts a comment to end of line (legal in multi-line
        // f-strings, PEP 701).
        let mut in_spec = false;
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
                    stack.push(b);
                    i += 1;
                }
                b')' => match stack.last() {
                    Some(b'(') => {
                        stack.pop();
                        i += 1;
                    }
                    Some(&open) => return Err(fstring_paren_mismatch(')', open, anchor)),
                    None => {
                        return Err(ParseError::Unexpected {
                            span: anchor,
                            message: "f-string: unmatched ')'".to_owned(),
                        })
                    }
                },
                b']' => match stack.last() {
                    Some(b'[') => {
                        stack.pop();
                        i += 1;
                    }
                    Some(&open) => return Err(fstring_paren_mismatch(']', open, anchor)),
                    None => {
                        return Err(ParseError::Unexpected {
                            span: anchor,
                            message: "f-string: unmatched ']'".to_owned(),
                        })
                    }
                },
                b'}' => match stack.last() {
                    None => return Ok((body[start..i].to_owned(), i)),
                    Some(b'{') => {
                        stack.pop();
                        i += 1;
                    }
                    Some(&open) => return Err(fstring_paren_mismatch('}', open, anchor)),
                },
                b':' if stack.is_empty() && !in_spec => {
                    in_spec = true;
                    i += 1;
                }
                b'#' if !in_spec => {
                    // Comment to end of line; the brackets/quotes it may
                    // contain must not perturb depth or string tracking.
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        Err(ParseError::Unexpected {
            span: anchor,
            message: "f-string: expecting '}'".to_owned(),
        })
    }

    /// Parse one `expr[!conv][:spec]` field and return a
    /// `FormattedValue` (possibly preceded by a synthetic literal
    /// for `{x = }` debug form).
    fn parse_fstring_field(
        &self,
        field: &str,
        anchor: Span,
        field_abs: u32,
        raw: bool,
        partial: bool,
    ) -> Result<Expr, ParseError> {
        // PEP 701 (3.12+): backslashes *are* allowed inside replacement
        // fields (e.g. `f"{d["a\tb"]}"`). The expression is re-tokenized
        // below, so escapes inside nested string literals are handled by
        // the sub-lexer; a stray backslash in the expression itself just
        // surfaces as a normal sub-parse error.
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
            // A `#` in the expression part (before any `!conv`/`:spec`,
            // and not inside a string) is a comment to end of line. Skip
            // it so quotes/`!`/`:` it contains can't be mistaken for
            // string delimiters or conv/spec boundaries.
            if in_str.is_none()
                && b == b'#'
                && conv_start.is_none()
                && spec_start.is_none()
            {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
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
                    // `!=` is the only `!` that stays part of the
                    // expression (comparison); any other `!` ends the
                    // expression and opens the conversion clause. Catching
                    // it here (rather than only before `s`/`r`/`a`) lets an
                    // empty expression before `!` surface CPython's
                    // "valid expression required before '!'".
                    if bytes.get(i + 1) != Some(&b'=') {
                        expr_end = i;
                        conv_start = Some(i + 1);
                        i += 1;
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
        let expr_slice = &field[..expr_end];
        // Debug form `{expr=}`: CPython echoes the *verbatim* source of
        // the expression part (preserving the author's whitespace, e.g.
        // `{val = }` -> "val = 7") and then formats the value. A trailing
        // single `=` triggers it, but `==`/`!=`/`<=`/`>=` must not.
        //
        // PEP 701 allows `#` comments inside (multi-line) replacement
        // fields, e.g.
        //   f"{1+2 = # my comment
        //     }"   ==  '1+2 = \n  3'
        // The comment is removed but the surrounding whitespace stays, and
        // it must not hide the debug `=`. Strip comments first, then both
        // the detection and the echoed literal work on the cleaned text.
        let clean = strip_fstring_field_comments(expr_slice);
        // Only ASCII whitespace is insignificant around the expression
        // (space, tab, formfeed, CR/LF, VT). Notably *not* U+00A0 etc. —
        // CPython rejects those as "invalid non-printable character".
        let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c');
        let trimmed_end = clean.trim_end_matches(ws);
        let is_debug = trimmed_end.ends_with('=')
            && !trimmed_end.ends_with("==")
            && !trimmed_end.ends_with("!=")
            && !trimmed_end.ends_with("<=")
            && !trimmed_end.ends_with(">=");
        let (expr_text, debug_lit) = if is_debug {
            let value_src = trimmed_end[..trimmed_end.len() - 1].trim_matches(ws);
            // Verbatim expression-part slice (through the `=`, including
            // any surrounding spaces, comments removed) is echoed.
            (value_src, Some(clean.clone()))
        } else {
            (clean.trim_matches(ws), None)
        };
        if expr_text.is_empty() {
            // Name the terminator that followed the (empty) expression,
            // mirroring CPython: "f-string: valid expression required
            // before '}'/'!'/':'/'='".
            let before = if is_debug {
                '='
            } else if conv_start.is_some() {
                '!'
            } else if spec_start.is_some() {
                ':'
            } else {
                '}'
            };
            // In a *partial* (unterminated) field there is no real `}`;
            // an all-whitespace field defers to the lexer's own
            // diagnostic ("expecting '}'" / "'{' was never closed").
            if partial && before == '}' {
                return Err(ParseError::Unexpected {
                    span: anchor,
                    message: "f-string: expecting '}'".to_owned(),
                });
            }
            return Err(ParseError::Unexpected {
                span: anchor,
                message: format!("f-string: valid expression required before '{before}'"),
            });
        }
        // Recursively tokenize+parse the expression. Inside an f-string
        // replacement field, newlines, comments and indentation are
        // insignificant (PEP 701: the field is parsed in the same
        // implicit line-continuation context as the surrounding `{...}`),
        // so a multi-line field like
        //   f'''{
        //   40  # forty
        //   + 2 # two
        //   }'''
        // must read as `40 + 2`. Wrapping the expression in parentheses
        // reproduces that joining exactly; the parens are transparent for
        // a plain expression (and for a top-level comma the result is the
        // same tuple `parse_expression_list` would have built). The
        // closing paren goes on its own line so a trailing `# comment` in
        // the field can't swallow it.
        // A failure parsing the embedded expression keeps its own
        // message and is re-anchored at the expression's *real* source
        // location (CPython's pegen reports interior errors verbatim —
        // e.g. "invalid decimal literal" inside `f'{(123_a)}'` points
        // at line 3 of a multi-line f-string, not at the `f`).
        // `expr_text` borrows `clean`; when no comments were stripped,
        // offsets in `clean` equal offsets in `field`.
        let expr_off_in_field = expr_text.as_ptr() as usize - clean.as_ptr() as usize;
        let expr_abs = field_abs + expr_off_in_field as u32;
        // Map a byte offset in `wrapped` (below) back to the source:
        // subtract the synthetic `(`, clamp to the expression's extent.
        let map_back = |byte: u32| -> u32 {
            let rel = (byte.saturating_sub(1)).min(expr_text.len() as u32);
            expr_abs + rel
        };
        // CPython's expression-start span: the first character of the
        // expression text.
        let expr_start_span = {
            let first_len = expr_text.chars().next().map_or(1, |c| c.len_utf8()) as u32;
            Span::new(expr_abs, expr_abs + first_len)
        };
        // CPython's invalid_replacement_field family of diagnostics:
        // a field whose expression can't be parsed *at all* is
        // "expecting a valid expression after '{'"; junk after a parsed
        // expression is "expecting '=', or '!', or ':', or '}'" — unless
        // the junk itself starts an expression, which pegen's
        // invalid_expression rule turns into the comma hint.
        let value = (|| -> Result<Expr, ParseError> {
            let wrapped = format!("({expr_text}\n)");
            let tokens = weavepy_lexer::tokenize(&wrapped)?;
            let mut sub = Parser::new(&wrapped, tokens);
            sub.skip_trivia_and_newlines();
            let value = sub.parse_expression_list(false)?;
            sub.skip_trivia_and_newlines();
            if !matches!(sub.peek(), TokenKind::Endmarker) {
                let sp = sub.peek_token().span;
                // Junk after a parsed expression: junk that itself begins
                // an expression gets pegen's comma hint (`f'{1 1}'`);
                // other junk names the expected delimiters.
                let junk_is_expr = sub.parse_expression_list(false).is_ok();
                return Err(ParseError::Unexpected {
                    span: sp,
                    message: if junk_is_expr {
                        "invalid syntax. Perhaps you forgot a comma?".to_owned()
                    } else {
                        "f-string: expecting '=', or '!', or ':', or '}'".to_owned()
                    },
                });
            }
            Ok(value)
        })()
        .map_err(|e| {
            map_fstring_subparse_error(
                e,
                expr_text,
                spec_start,
                field_abs,
                expr_abs,
                expr_start_span,
                &map_back,
            )
        })?;
        // Re-anchor the sub-parsed tree from `wrapped` coordinates onto
        // the real source so `ast` positions (lineno/col_offset) and
        // runtime tracebacks point into the actual f-string text.
        let mut value = value;
        remap_expr_spans(&mut value, &map_back);
        // A bare starred expression can't be formatted (`f'{*a}'`); the
        // tuple form `f'{*a,}'` is fine and parses as a tuple instead.
        if !partial {
            if let ExprKind::Starred(_) = value.kind {
                return Err(ParseError::Unexpected {
                    span: Span::new(expr_abs, expr_abs + expr_text.len() as u32),
                    message: "can't use starred expression here".to_owned(),
                });
            }
        }

        let conversion = match conv_start {
            Some(idx) if !partial => {
                let conv_end = spec_start.map(|s| s - 1).unwrap_or(field.len());
                let text = &field[idx..conv_end];
                validate_fstring_conversion(text, idx, field, field_abs)?
            }
            // Partial (unterminated) field whose conversion clause *was*
            // terminated by a `:` — the clause is complete enough to
            // validate (`f'{3!:'` is "missing conversion character", not
            // an unterminated-literal error).
            Some(idx) if spec_start.is_some() => {
                let conv_end = spec_start.map(|s| s - 1).unwrap_or(field.len());
                let text = &field[idx..conv_end];
                validate_fstring_conversion(text, idx, field, field_abs)?
            }
            // Partial field with the conversion clause itself cut off:
            // defer to the lexer's diagnostics.
            Some(_) => -1,
            // Debug form defaults to `!r`, but only when no explicit
            // conversion *and* no format spec is given (`{x=:.2f}` uses
            // the spec, not repr).
            None if debug_lit.is_some() && spec_start.is_none() => i32::from(b'r'),
            None => -1,
        };
        let format_spec = match spec_start {
            Some(s) if !partial => {
                let spec = &field[s..];
                let inner =
                    self.parse_fstring_body_inner(spec, raw, anchor, field_abs + s as u32, true)?;
                Some(Box::new(Expr {
                    kind: ExprKind::JoinedStr(inner),
                    span: anchor,
                }))
            }
            _ => None,
        };

        // CPython spans the FormattedValue from its `{` through the
        // matching `}` (the field text sits at `field_abs`, just past
        // the opening brace).
        let fv_span = Span::new(
            field_abs.saturating_sub(1),
            field_abs + field.len() as u32 + 1,
        );
        let fv = Expr {
            kind: ExprKind::FormattedValue {
                value: Box::new(value),
                conversion,
                format_spec,
            },
            span: fv_span,
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

/// Rewrite every span in an expression tree through `map` (a byte-offset
/// translation). Used to re-anchor an f-string replacement field's
/// sub-parsed expression from its synthetic `( … \n)` wrapper onto the
/// real source, so `ast` positions and runtime tracebacks point at the
/// actual f-string text (CPython parses fields in-place and gets this
/// for free).
fn remap_expr_spans(e: &mut Expr, map: &dyn Fn(u32) -> u32) {
    e.span = Span::new(map(e.span.start.0), map(e.span.end.0));
    let remap_opt = |o: &mut Option<Box<Expr>>, map: &dyn Fn(u32) -> u32| {
        if let Some(inner) = o {
            remap_expr_spans(inner, map);
        }
    };
    let remap_args = |a: &mut crate::ast::Arguments, map: &dyn Fn(u32) -> u32| {
        for arg in a
            .posonlyargs
            .iter_mut()
            .chain(a.args.iter_mut())
            .chain(a.kwonlyargs.iter_mut())
            .chain(a.vararg.iter_mut())
            .chain(a.kwarg.iter_mut())
        {
            arg.span = Span::new(map(arg.span.start.0), map(arg.span.end.0));
            if let Some(ann) = &mut arg.annotation {
                remap_expr_spans(ann, map);
            }
        }
        for d in &mut a.defaults {
            remap_expr_spans(d, map);
        }
        for d in a.kw_defaults.iter_mut().flatten() {
            remap_expr_spans(d, map);
        }
    };
    let remap_comps = |gens: &mut Vec<crate::ast::Comprehension>, map: &dyn Fn(u32) -> u32| {
        for g in gens {
            remap_expr_spans(&mut g.target, map);
            remap_expr_spans(&mut g.iter, map);
            for i in &mut g.ifs {
                remap_expr_spans(i, map);
            }
        }
    };
    match &mut e.kind {
        ExprKind::Constant(_) | ExprKind::Name(_) => {}
        ExprKind::Attribute { value, .. } => remap_expr_spans(value, map),
        ExprKind::Subscript { value, slice } => {
            remap_expr_spans(value, map);
            remap_expr_spans(slice, map);
        }
        ExprKind::Slice { lower, upper, step } => {
            remap_opt(lower, map);
            remap_opt(upper, map);
            remap_opt(step, map);
        }
        ExprKind::BinOp { left, right, .. } => {
            remap_expr_spans(left, map);
            remap_expr_spans(right, map);
        }
        ExprKind::BoolOp { values, .. } => {
            for v in values {
                remap_expr_spans(v, map);
            }
        }
        ExprKind::UnaryOp { operand, .. } => remap_expr_spans(operand, map),
        ExprKind::Compare {
            left, comparators, ..
        } => {
            remap_expr_spans(left, map);
            for c in comparators {
                remap_expr_spans(c, map);
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            remap_expr_spans(test, map);
            remap_expr_spans(body, map);
            remap_expr_spans(orelse, map);
        }
        ExprKind::NamedExpr { target, value } => {
            remap_expr_spans(target, map);
            remap_expr_spans(value, map);
        }
        ExprKind::Lambda { args, body } => {
            remap_args(args, map);
            remap_expr_spans(body, map);
        }
        ExprKind::Call {
            func,
            args,
            keywords,
        } => {
            remap_expr_spans(func, map);
            for a in args {
                remap_expr_spans(a, map);
            }
            for k in keywords {
                remap_expr_spans(&mut k.value, map);
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) | ExprKind::Set(items) => {
            for i in items {
                remap_expr_spans(i, map);
            }
        }
        ExprKind::Dict { keys, values } => {
            for k in keys.iter_mut().flatten() {
                remap_expr_spans(k, map);
            }
            for v in values {
                remap_expr_spans(v, map);
            }
        }
        ExprKind::ListComp { elt, generators }
        | ExprKind::SetComp { elt, generators }
        | ExprKind::GeneratorExp { elt, generators } => {
            remap_expr_spans(elt, map);
            remap_comps(generators, map);
        }
        ExprKind::DictComp {
            key,
            value,
            generators,
        } => {
            remap_expr_spans(key, map);
            remap_expr_spans(value, map);
            remap_comps(generators, map);
        }
        ExprKind::Starred(inner)
        | ExprKind::YieldFrom(inner)
        | ExprKind::Await(inner) => remap_expr_spans(inner, map),
        ExprKind::Yield(inner) => {
            if let Some(i) = inner {
                remap_expr_spans(i, map);
            }
        }
        ExprKind::JoinedStr(parts) => {
            for p in parts {
                remap_expr_spans(p, map);
            }
        }
        ExprKind::FormattedValue {
            value, format_spec, ..
        } => {
            remap_expr_spans(value, map);
            remap_opt(format_spec, map);
        }
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

/// Build CPython's "closing parenthesis 'X' does not match opening
/// parenthesis 'Y'" diagnostic for a mismatched bracket inside an f-string
/// replacement field.
fn fstring_paren_mismatch(close: char, open: u8, anchor: Span) -> ParseError {
    ParseError::Unexpected {
        span: anchor,
        message: format!(
            "closing parenthesis '{close}' does not match opening parenthesis '{}'",
            open as char
        ),
    }
}

/// Map a failure from the replacement-field sub-parse onto CPython's
/// pegen diagnostics. Specialized messages (the comma hint, a nested
/// f-string's own error, …) survive verbatim; internal phrasing is
/// re-derived: if *some* leading expression parses the field has junk
/// after it ("expecting '=', or '!', or ':', or '}'"), if a lambda lost
/// its `:` to the format spec it gets the dedicated lambda message, and
/// otherwise the expression never started ("expecting a valid expression
/// after '{'").
#[allow(clippy::too_many_arguments)]
fn map_fstring_subparse_error(
    e: ParseError,
    expr_text: &str,
    spec_start: Option<usize>,
    field_abs: u32,
    expr_abs: u32,
    expr_start_span: Span,
    map_back: &dyn Fn(u32) -> u32,
) -> ParseError {
    let valid_expr_err = || ParseError::Unexpected {
        span: expr_start_span,
        message: "f-string: expecting a valid expression after '{'".to_owned(),
    };
    // Does a non-empty leading slice of the expression parse as a complete
    // expression list? Returns the junk position (in `wrapped`
    // coordinates) when one does and junk follows.
    let leading_parse = |text: &str| -> Option<(u32, bool)> {
        let wrapped = format!("({text}\n)");
        let tokens = weavepy_lexer::tokenize(&wrapped).ok()?;
        let mut sub = Parser::new(&wrapped, tokens);
        sub.skip_trivia_and_newlines();
        // Consume the synthetic `(` so the parse isn't forced to reach
        // the matching `)`.
        sub.bump();
        sub.skip_trivia_and_newlines();
        sub.parse_expression_list(false).ok()?;
        sub.skip_trivia_and_newlines();
        let sp = sub.peek_token().span;
        let junk_is_expr = sub.parse_expression_list(false).is_ok();
        Some((sp.start.0, junk_is_expr))
    };
    match e {
        ParseError::Lex(weavepy_lexer::LexError::InvalidToken { pos }) => {
            // A byte the tokenizer can't start a token with (`$`, `?`,
            // `` ` ``): pegen sees an error token where it expects
            // `=`/`!`/`:`/`}` — provided some expression precedes it.
            let prefix_end = (pos as usize).saturating_sub(1).min(expr_text.len());
            let prefix = expr_text[..prefix_end].trim();
            let at = map_back(pos);
            if !prefix.is_empty() && leading_parse(prefix).is_some() {
                ParseError::Unexpected {
                    span: Span::new(at, at + 1),
                    message: "f-string: expecting '=', or '!', or ':', or '}'".to_owned(),
                }
            } else {
                valid_expr_err()
            }
        }
        ParseError::Lex(lex) => {
            let at = map_back(lex.byte_offset());
            ParseError::Unexpected {
                span: Span::new(at, at),
                message: lex.to_string(),
            }
        }
        ParseError::Unexpected { span, message } | ParseError::Indentation { span, message } => {
            let internal = message.starts_with("expected ")
                || message.starts_with("unexpected token")
                || message == "trailing";
            if !internal {
                return ParseError::Unexpected {
                    span: Span::new(map_back(span.start.0), map_back(span.end.0)),
                    message,
                };
            }
            match leading_parse(expr_text) {
                // A leading expression parses; the failure is junk after
                // it (the junk may itself be a `(`-less continuation the
                // paren-wrapped parse choked on).
                Some((junk_pos, junk_is_expr)) => {
                    let at = map_back(junk_pos);
                    ParseError::Unexpected {
                        span: Span::new(at, at + 1),
                        message: if junk_is_expr {
                            "invalid syntax. Perhaps you forgot a comma?".to_owned()
                        } else {
                            "f-string: expecting '=', or '!', or ':', or '}'".to_owned()
                        },
                    }
                }
                None => fstring_lambda_error(expr_text, spec_start, field_abs, expr_abs)
                    .unwrap_or_else(valid_expr_err),
            }
        }
        ParseError::NotImplemented { .. } => valid_expr_err(),
    }
}

/// CPython's pegen has a dedicated rule for a lambda whose body `:` was
/// consumed as the format-spec separator: `f'{lambda x:x}'`. Detected by
/// re-attaching a synthetic `: 0` body — if that parses, the original
/// failure was exactly the missing colon.
fn fstring_lambda_error(
    expr_text: &str,
    spec_start: Option<usize>,
    field_abs: u32,
    expr_abs: u32,
) -> Option<ParseError> {
    let spec_start = spec_start?;
    if !expr_text.contains("lambda") {
        return None;
    }
    let test = format!("({expr_text}: 0\n)");
    let tokens = weavepy_lexer::tokenize(&test).ok()?;
    let mut sub = Parser::new(&test, tokens);
    sub.skip_trivia_and_newlines();
    sub.parse_expression_list(false).ok()?;
    sub.skip_trivia_and_newlines();
    if !matches!(sub.peek(), TokenKind::Endmarker) {
        return None;
    }
    let lam_off = find_last_toplevel_lambda(expr_text)?;
    Some(ParseError::Unexpected {
        // CPython spans from the `lambda` keyword through the spec colon.
        span: Span::new(expr_abs + lam_off as u32, field_abs + spec_start as u32),
        message: "f-string: lambda expressions are not allowed without parentheses".to_owned(),
    })
}

/// Byte offset of the last top-level `lambda` keyword in `expr` (outside
/// strings and brackets, with identifier boundaries).
fn find_last_toplevel_lambda(expr: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut found = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'l' if depth == 0 => {
                let is_word = bytes[i..].starts_with(b"lambda")
                    && (i == 0 || !is_ident_byte(bytes[i - 1]))
                    && bytes.get(i + 6).is_none_or(|&n| !is_ident_byte(n));
                if is_word {
                    found = Some(i);
                    i += 6;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Validate the conversion clause text (between `!` and the spec `:` /
/// field end), returning the conversion code or CPython's exact
/// diagnostic. `idx` is the clause's byte offset within `field`.
fn validate_fstring_conversion(
    text: &str,
    idx: usize,
    field: &str,
    field_abs: u32,
) -> Result<i32, ParseError> {
    let abs = |off: usize| field_abs + (idx + off) as u32;
    if text.is_empty() {
        return Err(ParseError::Unexpected {
            span: Span::new(abs(0), abs(0) + 1),
            message: "f-string: missing conversion character".to_owned(),
        });
    }
    let first = text.chars().next().expect("non-empty");
    if first.is_whitespace() {
        // CPython spans from the `!` through the end of the field.
        return Err(ParseError::Unexpected {
            span: Span::new(
                field_abs + (idx - 1) as u32,
                field_abs + field.len() as u32,
            ),
            message: "f-string: conversion type must come right after the exclamation mark"
                .to_owned(),
        });
    }
    if !(unicode_ident::is_xid_start(first) || first == '_') {
        return Err(ParseError::Unexpected {
            span: Span::new(abs(0), abs(first.len_utf8())),
            message: "f-string: invalid conversion character".to_owned(),
        });
    }
    let ident_end = text
        .char_indices()
        .find(|(_, c)| !(unicode_ident::is_xid_continue(*c) || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let ident = &text[..ident_end];
    // Trailing whitespace after the conversion is insignificant; anything
    // else expects the spec or the closing brace.
    let rest = &text[ident_end..];
    if let Some((j, c)) = rest.char_indices().find(|(_, c)| !c.is_whitespace()) {
        let at = ident_end + j;
        return Err(ParseError::Unexpected {
            span: Span::new(abs(at), abs(at + c.len_utf8())),
            message: "f-string: expecting ':' or '}'".to_owned(),
        });
    }
    if !matches!(ident, "s" | "r" | "a") {
        return Err(ParseError::Unexpected {
            span: Span::new(abs(0), abs(ident_end)),
            message: format!(
                "f-string: invalid conversion character '{ident}': expected 's', 'r', or 'a'"
            ),
        });
    }
    Ok(i32::from(ident.as_bytes()[0]))
}

/// Remove `#` comments from the expression part of an f-string replacement
/// field while leaving everything else (including whitespace and newlines)
/// byte-for-byte intact. PEP 701 permits comments inside multi-line fields;
/// a `#` only starts a comment outside of string literals, so this tracks
/// single/triple-quoted strings (and their backslash escapes) to avoid
/// mangling a `#` that lives inside a string. A comment runs to the next
/// newline (the newline itself is preserved).
fn strip_fstring_field_comments(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    // `Some(quote)` while inside a string literal; `triple` tracks `"""`/`'''`.
    let mut in_str: Option<u8> = None;
    let mut triple = false;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                // Copy the backslash and its escaped char as a unit so an
                // escaped quote can't be read as closing the string.
                out.push('\\');
                i += 1;
                if i < bytes.len() {
                    let cl = utf8_char_len(bytes[i]);
                    let e = (i + cl).min(bytes.len());
                    out.push_str(&s[i..e]);
                    i = e;
                }
                continue;
            }
            if b == q {
                if triple {
                    if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                        out.push_str(&s[i..i + 3]);
                        i += 3;
                        in_str = None;
                        triple = false;
                        continue;
                    }
                } else {
                    out.push(q as char);
                    i += 1;
                    in_str = None;
                    continue;
                }
            }
            let cl = utf8_char_len(b);
            let e = (i + cl).min(bytes.len());
            out.push_str(&s[i..e]);
            i = e;
            continue;
        }
        match b {
            b'#' => {
                // Drop the comment up to (but not including) the newline.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                if i + 2 < bytes.len() && bytes[i + 1] == b && bytes[i + 2] == b {
                    in_str = Some(b);
                    triple = true;
                    out.push_str(&s[i..i + 3]);
                    i += 3;
                } else {
                    in_str = Some(b);
                    triple = false;
                    out.push(b as char);
                    i += 1;
                }
            }
            _ => {
                let cl = utf8_char_len(b);
                let e = (i + cl).min(bytes.len());
                out.push_str(&s[i..e]);
                i = e;
            }
        }
    }
    out
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
            // The merged literal spans from the first fragment's start
            // to the last fragment's end (CPython's implicit-concat AST
            // positions cross the token boundary).
            last.span = Span::new(last.span.start.0, span.end.0);
            return;
        }
    }
    parts.push(Expr {
        kind: ExprKind::Constant(Constant::Str(s)),
        span,
    });
}

/// Re-parse an unterminated f-string field (`source[field_start..limit]`)
/// to surface a *specialized* interior diagnostic, if any. Returns
/// `None` when the partial expression parses cleanly or only fails
/// generically — the caller then keeps the original lexer error.
pub(crate) fn partial_fstring_field_error(
    source: &str,
    field_start: u32,
    limit: u32,
) -> Option<ParseError> {
    let end = (limit as usize).min(source.len());
    let start = (field_start as usize).min(end);
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return None;
    }
    let field = &source[start..end];
    let tokens = weavepy_lexer::tokenize("").ok()?;
    let p = Parser::new("", tokens);
    let anchor = Span::new(field_start, limit);
    match p.parse_fstring_field(field, anchor, field_start, false, true) {
        Err(e) => {
            // Errors CPython's pegen reports from the tokens it has
            // already seen win over the tokenizer's unterminated-field
            // diagnostic; anything else keeps the lexer error.
            let wins = match &e {
                ParseError::Unexpected { message, .. }
                | ParseError::Indentation { message, .. } => {
                    message.starts_with("invalid syntax.")
                        || message.starts_with("f-string: valid expression required before")
                        || message == "f-string: expecting a valid expression after '{'"
                        || message == "f-string: expecting '=', or '!', or ':', or '}'"
                        || message == "f-string: missing conversion character"
                        || message.starts_with("f-string: invalid conversion character")
                        || message
                            == "f-string: conversion type must come right after the exclamation mark"
                        || message
                            == "f-string: lambda expressions are not allowed without parentheses"
                }
                _ => false,
            };
            wins.then_some(e)
        }
        Ok(_) => None,
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

/// Decode a (non-f) string-literal body. Returns the decoded text plus
/// any invalid-escape diagnostics CPython would surface as a
/// `SyntaxWarning` (unrecognised escapes and octal escapes `> \377`).
/// Each diagnostic carries the byte offset of its backslash *within the
/// body* so the caller can map it back to an absolute source position.
fn decode_str_body(s: &str, raw: bool) -> Result<String, String> {
    if raw {
        return Ok(s.to_owned());
    }
    // CPython surfaces escape-decoding failures as the unicodeescape
    // codec's error, with byte positions of the failed escape within the
    // body: "(unicode error) 'unicodeescape' codec can't decode bytes in
    // position {start}-{end}: {reason}". `end` is the last consumed byte.
    let unicode_err = |start: usize, end: usize, reason: &str| -> String {
        let last = end.saturating_sub(1);
        format!(
            "(unicode error) 'unicodeescape' codec can't decode bytes in \
             position {start}-{last}: {reason}"
        )
    };
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((bs, c)) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some((_, esc)) = chars.next() else {
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
            // Octal escape `\ooo`: 1–3 octal digits (CPython accepts up
            // to `\777` = 511 in a str literal). `\0` is just the
            // zero-length-tail case of this rule. Values above `\377`
            // draw a `SyntaxWarning`, detected by the lexer.
            '0'..='7' => {
                let mut val = esc as u32 - '0' as u32;
                for _ in 0..2 {
                    match chars.peek().copied() {
                        Some((_, d @ '0'..='7')) => {
                            val = val * 8 + (d as u32 - '0' as u32);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(val).unwrap_or('\u{FFFD}'));
            }
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            // Line continuation inside the string (`\` + newline). A
            // `\<CR>` (optionally `\<CR><LF>`) continues too.
            '\n' => {}
            '\r' => {
                if matches!(chars.peek(), Some((_, '\n'))) {
                    chars.next();
                }
            }
            'x' | 'u' | 'U' => {
                let want = match esc {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let mut hex = String::new();
                for _ in 0..want {
                    match chars.peek().copied() {
                        Some((_, h)) if h.is_ascii_hexdigit() => {
                            hex.push(h);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                let end = chars.peek().map_or(bytes.len(), |(i, _)| *i);
                if hex.len() < want {
                    let reason = match esc {
                        'x' => "truncated \\xXX escape",
                        'u' => "truncated \\uXXXX escape",
                        _ => "truncated \\UXXXXXXXX escape",
                    };
                    return Err(unicode_err(bs, end, reason));
                }
                let n = u32::from_str_radix(&hex, 16).expect("hex digits");
                // Out-of-range code points are CPython's "illegal Unicode
                // character". Lone surrogates are *valid* in CPython str
                // literals; Rust `char` can't hold them, so they degrade
                // to U+FFFD (pre-existing representation limit).
                if n > 0x10FFFF {
                    return Err(unicode_err(bs, end, "illegal Unicode character"));
                }
                out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
            }
            'N' => {
                // `\N{UNICODE CHARACTER NAME}` — resolve the name against
                // the full UCD name table. CPython requires the brace form
                // and raises a SyntaxError ("malformed \N character escape"
                // / "unknown Unicode character name") otherwise.
                if !matches!(chars.peek(), Some((_, '{'))) {
                    return Err(unicode_err(bs, bs + 2, "malformed \\N character escape"));
                }
                chars.next();
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some((i, '}')) => {
                            let ch = unicode_names2::character(&name).ok_or_else(|| {
                                unicode_err(bs, i + 1, "unknown Unicode character name")
                            })?;
                            out.push(ch);
                            break;
                        }
                        Some((_, c)) => name.push(c),
                        None => {
                            return Err(unicode_err(
                                bs,
                                bytes.len(),
                                "malformed \\N character escape",
                            ))
                        }
                    }
                }
            }
            other => {
                // CPython issues a `SyntaxWarning` for unknown escapes (the
                // lexer records it) but emits both characters literally.
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Decode a bytes-literal body. Like [`decode_str_body`] but bytes-valued
/// and ASCII-only: a non-ASCII source character is a `SyntaxError` ("bytes
/// can only contain ASCII literal characters") in both raw and cooked
/// forms, and octal escapes wrap mod 256. Invalid-escape `SyntaxWarning`s
/// are detected separately by the lexer (see
/// [`weavepy_lexer::tokenize_with_escapes`]).
fn decode_bytes_body(s: &str, raw: bool) -> Result<Vec<u8>, String> {
    if raw {
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            if !c.is_ascii() {
                return Err("bytes can only contain ASCII literal characters".to_owned());
            }
            out.push(c as u8);
        }
        return Ok(out);
    }
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii() {
            if c != '\\' {
                out.push(c as u8);
                continue;
            }
        } else {
            return Err("bytes can only contain ASCII literal characters".to_owned());
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
            // Octal escape `\ooo` (1–3 digits). In a bytes literal the
            // value is stored as a single byte, so CPython wraps it mod
            // 256 (`b'\400'` -> 0x00, `b'\777'` -> 0xFF).
            '0'..='7' => {
                let mut val: u32 = esc as u32 - '0' as u32;
                for _ in 0..2 {
                    match chars.peek().copied() {
                        Some(d @ '0'..='7') => {
                            val = val * 8 + (d as u32 - '0' as u32);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.push((val & 0xFF) as u8);
            }
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
    use num_bigint::BigInt;

    let cleaned: String = lex.chars().filter(|c| *c != '_').collect();

    // Imaginary suffix: peel `j`/`J` and parse the body as a float.
    if cleaned.ends_with('j') || cleaned.ends_with('J') {
        let body = &cleaned[..cleaned.len() - 1];
        let imag: f64 = body
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())?;
        return Ok(Constant::Complex(0.0, imag));
    }

    // Integer literal in a non-decimal radix.
    let try_radix = |prefix_lo: &str, prefix_hi: &str, radix: u32| -> Option<&str> {
        let _ = radix;
        cleaned
            .strip_prefix(prefix_lo)
            .or_else(|| cleaned.strip_prefix(prefix_hi))
    };
    if let Some(rest) = try_radix("0x", "0X", 16) {
        return parse_radix_int(rest, 16);
    }
    if let Some(rest) = try_radix("0o", "0O", 8) {
        return parse_radix_int(rest, 8);
    }
    if let Some(rest) = try_radix("0b", "0B", 2) {
        return parse_radix_int(rest, 2);
    }

    // Float literal.
    let has_float_marker = cleaned.contains('.') || cleaned.contains('e') || cleaned.contains('E');
    if has_float_marker {
        let f: f64 = cleaned
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())?;
        return Ok(Constant::Float(f));
    }

    // Decimal integer; promote to BigInt on overflow.
    if let Ok(n) = cleaned.parse::<i64>() {
        return Ok(Constant::Int(n));
    }
    let big: BigInt = cleaned
        .parse()
        .map_err(|_| "invalid integer literal".to_owned())?;
    if let Some(small) = big_to_i64(&big) {
        return Ok(Constant::Int(small));
    }
    Ok(Constant::BigInt(big.to_string()))
}

fn parse_radix_int(rest: &str, radix: u32) -> Result<Constant, String> {
    use num_bigint::BigInt;

    if let Ok(n) = i64::from_str_radix(rest, radix) {
        return Ok(Constant::Int(n));
    }
    let big = BigInt::parse_bytes(rest.as_bytes(), radix)
        .ok_or_else(|| "invalid integer literal".to_owned())?;
    if let Some(small) = big_to_i64(&big) {
        return Ok(Constant::Int(small));
    }
    Ok(Constant::BigInt(big.to_string()))
}

fn big_to_i64(b: &num_bigint::BigInt) -> Option<i64> {
    use num_bigint::Sign;
    let (sign, digits) = b.to_u64_digits();
    match digits.len() {
        0 => Some(0),
        1 => {
            let v = digits[0];
            match sign {
                Sign::Plus | Sign::NoSign => i64::try_from(v).ok(),
                Sign::Minus => {
                    if v == (i64::MAX as u64) + 1 {
                        Some(i64::MIN)
                    } else {
                        i64::try_from(v).ok().map(|n| -n)
                    }
                }
            }
        }
        _ => None,
    }
}

/// PEP 695 helper — wrap `body` in a `(lambda T, U: body)(TypeVar('T'), TypeVar('U'))`
/// call so type-parameter names bind locally to typevar instances.
fn wrap_in_type_param_lambda(body: Expr, names: &[String], span: Span) -> Expr {
    let args = Arguments {
        posonlyargs: Vec::new(),
        args: names
            .iter()
            .map(|n| Arg {
                name: n.clone(),
                annotation: None,
                span,
            })
            .collect(),
        vararg: None,
        kwonlyargs: Vec::new(),
        kw_defaults: Vec::new(),
        kwarg: None,
        defaults: Vec::new(),
    };
    let lambda = Expr {
        kind: ExprKind::Lambda {
            args,
            body: Box::new(body),
        },
        span,
    };
    let typevar_calls: Vec<Expr> = names
        .iter()
        .map(|n| Expr {
            kind: ExprKind::Call {
                func: Box::new(Expr {
                    kind: ExprKind::Name("__weavepy_typevar__".to_owned()),
                    span,
                }),
                args: vec![Expr {
                    kind: ExprKind::Constant(Constant::Str(n.clone())),
                    span,
                }],
                keywords: Vec::new(),
            },
            span,
        })
        .collect();
    Expr {
        kind: ExprKind::Call {
            func: Box::new(lambda),
            args: typevar_calls,
            keywords: Vec::new(),
        },
        span,
    }
}
