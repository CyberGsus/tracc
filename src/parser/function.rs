use super::{Parse, ParseRes, Parser};
use crate::ast::Function;
use crate::lexer::TokenKind;

impl Parse for Function {
    fn parse(parser: &mut Parser) -> ParseRes<Self> {
        parser.with_context("parsing function", |parser| {
            parser.keyword("int")?;
            let name = parser.parse()?;
            parser.expect_token(TokenKind::OpenParen)?;
            parser.accept_current();
            parser.expect_token(TokenKind::CloseParen)?;
            parser.accept_current();
            parser.expect_token(TokenKind::OpenBrace)?;
            parser.accept_current();
            parser.keyword("return")?;
            let return_expr = parser.parse()?;
            parser.expect_token(TokenKind::Semicolon)?;
            parser.accept_current();
            parser.expect_token(TokenKind::CloseBrace)?;
            parser.accept_current();
            Ok(Self { name, return_expr })
        })
    }
}
