use crate::ast::*;
use crate::lexer::{Keyword, Token, TokenKind, tokenize};
use crate::types::Value;
use crate::{Error, Result};

pub fn parse_sql(input: &str) -> Result<Statement> {
    Parser {
        tokens: tokenize(input)?,
        current: 0,
    }
    .parse_statement()
}

struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    fn parse_statement(mut self) -> Result<Statement> {
        let statement = if self.consume_keyword(Keyword::Explain) {
            let physical = self.consume_keyword(Keyword::Physical);
            if !self.check_keyword(Keyword::Select) {
                return self.error("EXPLAIN currently supports SELECT only");
            }
            Statement::Explain {
                physical,
                statement: Box::new(Statement::Select(self.parse_select()?)),
            }
        } else if self.check_keyword(Keyword::Select) {
            Statement::Select(self.parse_select()?)
        } else {
            return self.error(
                "expected SELECT or EXPLAIN; DDL and mutation statements are not supported",
            );
        };
        self.consume(&TokenKind::Semicolon);
        if !self.check(&TokenKind::Eof) {
            return self.error("expected end of statement");
        }
        Ok(statement)
    }

    fn parse_select(&mut self) -> Result<Select> {
        self.expect_keyword(Keyword::Select)?;
        let mut projection = Vec::new();
        loop {
            if self.consume(&TokenKind::Star) {
                projection.push(SelectItem::Wildcard);
            } else {
                let expr = self.parse_expr(0)?;
                let alias = if self.consume_keyword(Keyword::As) {
                    Some(self.expect_ident("expected alias after AS")?)
                } else if matches!(self.peek().kind, TokenKind::Ident(_)) {
                    Some(self.expect_ident("expected alias")?)
                } else {
                    None
                };
                projection.push(SelectItem::Expr { expr, alias });
            }
            if !self.consume(&TokenKind::Comma) {
                break;
            }
        }
        self.expect_keyword(Keyword::From)?;
        let from = self.parse_table_ref()?;
        let mut joins = Vec::new();
        while self.consume_keyword(Keyword::Inner) || self.check_keyword(Keyword::Join) {
            self.expect_keyword(Keyword::Join)?;
            let table = self.parse_table_ref()?;
            self.expect_keyword(Keyword::On)?;
            joins.push(Join {
                table,
                on: self.parse_expr(0)?,
            });
        }
        let selection = if self.consume_keyword(Keyword::Where) {
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        let group_by = if self.consume_keyword(Keyword::Group) {
            self.expect_keyword(Keyword::By)?;
            self.parse_expr_list()?
        } else {
            Vec::new()
        };
        let order_by = if self.consume_keyword(Keyword::Order) {
            self.expect_keyword(Keyword::By)?;
            let mut result = Vec::new();
            loop {
                let expr = self.parse_expr(0)?;
                let asc = if self.consume_keyword(Keyword::Desc) {
                    false
                } else {
                    self.consume_keyword(Keyword::Asc);
                    true
                };
                result.push(OrderBy { expr, asc });
                if !self.consume(&TokenKind::Comma) {
                    break;
                }
            }
            result
        } else {
            Vec::new()
        };
        let limit = if self.consume_keyword(Keyword::Limit) {
            let token = self.advance().clone();
            match token.kind {
                TokenKind::Number(value) if !value.contains('.') => {
                    Some(value.parse().map_err(|_| Error::Parse {
                        position: token.position,
                        message: "LIMIT is too large".into(),
                    })?)
                }
                _ => {
                    return Err(Error::Parse {
                        position: token.position,
                        message: "LIMIT requires a non-negative integer".into(),
                    });
                }
            }
        } else {
            None
        };
        Ok(Select {
            projection,
            from,
            joins,
            selection,
            group_by,
            order_by,
            limit,
        })
    }

    fn parse_expr_list(&mut self) -> Result<Vec<Expr>> {
        let mut expressions = vec![self.parse_expr(0)?];
        while self.consume(&TokenKind::Comma) {
            expressions.push(self.parse_expr(0)?);
        }
        Ok(expressions)
    }

    fn parse_table_ref(&mut self) -> Result<TableRef> {
        let name = self.expect_ident("expected table name")?;
        let alias = if self.consume_keyword(Keyword::As) {
            Some(self.expect_ident("expected table alias after AS")?)
        } else if matches!(self.peek().kind, TokenKind::Ident(_)) {
            Some(self.expect_ident("expected table alias")?)
        } else {
            None
        };
        Ok(TableRef { name, alias })
    }

    fn parse_expr(&mut self, min_precedence: u8) -> Result<Expr> {
        let mut left = self.parse_prefix()?;
        loop {
            let Some((op, precedence)) = self.binary_op() else {
                break;
            };
            if precedence < min_precedence {
                break;
            }
            self.advance();
            let right = self.parse_expr(precedence + 1)?;
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_prefix(&mut self) -> Result<Expr> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Number(value) if value.contains('.') => value
                .parse::<f64>()
                .map(|v| Expr::Literal(Value::Float64(v)))
                .map_err(|_| Error::Parse {
                    position: token.position,
                    message: "invalid floating-point literal".into(),
                }),
            TokenKind::Number(value) => value
                .parse::<i64>()
                .map(|v| Expr::Literal(Value::Int64(v)))
                .map_err(|_| Error::Parse {
                    position: token.position,
                    message: "integer literal is out of range".into(),
                }),
            TokenKind::String(value) => Ok(Expr::Literal(Value::Utf8(value))),
            TokenKind::Keyword(Keyword::True) => Ok(Expr::Literal(Value::Boolean(true))),
            TokenKind::Keyword(Keyword::False) => Ok(Expr::Literal(Value::Boolean(false))),
            TokenKind::Keyword(Keyword::Null) => Ok(Expr::Literal(Value::Null)),
            TokenKind::Keyword(Keyword::Not) => Ok(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(self.parse_expr(7)?),
            }),
            TokenKind::Minus => Ok(Expr::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(self.parse_expr(7)?),
            }),
            TokenKind::LParen => {
                let expr = self.parse_expr(0)?;
                self.expect(&TokenKind::RParen, "expected ')' after expression")?;
                Ok(expr)
            }
            TokenKind::Ident(name) => self.parse_column(name),
            TokenKind::Keyword(
                function @ (Keyword::Count
                | Keyword::Sum
                | Keyword::Avg
                | Keyword::Min
                | Keyword::Max),
            ) => {
                self.expect(&TokenKind::LParen, "expected '(' after aggregate function")?;
                let expr = if self.consume(&TokenKind::Star) {
                    Expr::Wildcard
                } else {
                    self.parse_expr(0)?
                };
                self.expect(&TokenKind::RParen, "expected ')' after aggregate argument")?;
                Ok(Expr::Aggregate {
                    function: match function {
                        Keyword::Count => AggregateFunction::Count,
                        Keyword::Sum => AggregateFunction::Sum,
                        Keyword::Avg => AggregateFunction::Avg,
                        Keyword::Min => AggregateFunction::Min,
                        Keyword::Max => AggregateFunction::Max,
                        _ => unreachable!(),
                    },
                    expr: Box::new(expr),
                })
            }
            _ => Err(Error::Parse {
                position: token.position,
                message: "expected expression".into(),
            }),
        }
    }

    fn parse_column(&mut self, first: String) -> Result<Expr> {
        if self.consume(&TokenKind::Dot) {
            let name = self.expect_ident("expected column name after '.'")?;
            Ok(Expr::Column {
                qualifier: Some(first),
                name,
            })
        } else {
            Ok(Expr::Column {
                qualifier: None,
                name: first,
            })
        }
    }

    fn binary_op(&self) -> Option<(BinaryOp, u8)> {
        Some(match self.peek().kind {
            TokenKind::Keyword(Keyword::Or) => (BinaryOp::Or, 1),
            TokenKind::Keyword(Keyword::And) => (BinaryOp::And, 2),
            TokenKind::Eq => (BinaryOp::Eq, 3),
            TokenKind::NotEq => (BinaryOp::NotEq, 3),
            TokenKind::Lt => (BinaryOp::Lt, 3),
            TokenKind::LtEq => (BinaryOp::LtEq, 3),
            TokenKind::Gt => (BinaryOp::Gt, 3),
            TokenKind::GtEq => (BinaryOp::GtEq, 3),
            TokenKind::Plus => (BinaryOp::Add, 4),
            TokenKind::Minus => (BinaryOp::Sub, 4),
            TokenKind::Star => (BinaryOp::Mul, 5),
            TokenKind::Slash => (BinaryOp::Div, 5),
            _ => return None,
        })
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }
    fn advance(&mut self) -> &Token {
        let i = self.current;
        self.current += 1;
        &self.tokens[i]
    }
    fn check(&self, kind: &TokenKind) -> bool {
        &self.peek().kind == kind
    }
    fn check_keyword(&self, keyword: Keyword) -> bool {
        self.peek().kind == TokenKind::Keyword(keyword)
    }
    fn consume(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn consume_keyword(&mut self, keyword: Keyword) -> bool {
        if self.check_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: &TokenKind, message: &str) -> Result<()> {
        if self.consume(kind) {
            Ok(())
        } else {
            self.error(message)
        }
    }
    fn expect_keyword(&mut self, keyword: Keyword) -> Result<()> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            self.error(&format!("expected {keyword:?}"))
        }
    }
    fn expect_ident(&mut self, message: &str) -> Result<String> {
        let token = self.advance().clone();
        match token.kind {
            TokenKind::Ident(value) => Ok(value),
            _ => Err(Error::Parse {
                position: token.position,
                message: message.into(),
            }),
        }
    }
    fn error<T>(&self, message: &str) -> Result<T> {
        Err(Error::Parse {
            position: self.peek().position,
            message: message.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(sql: &str) -> Select {
        match parse_sql(sql).unwrap() {
            Statement::Select(value) => value,
            _ => panic!("not SELECT"),
        }
    }

    #[test]
    fn parses_complete_select() {
        let query = select(
            "SELECT c.region, SUM(o.total) AS revenue FROM customers c INNER JOIN orders o ON c.id = o.customer_id WHERE o.total > 10 GROUP BY c.region ORDER BY revenue DESC LIMIT 5;",
        );
        assert_eq!(query.projection.len(), 2);
        assert_eq!(query.joins.len(), 1);
        assert_eq!(query.group_by.len(), 1);
        assert!(!query.order_by[0].asc);
        assert_eq!(query.limit, Some(5));
    }

    #[test]
    fn respects_expression_precedence() {
        let query = select("SELECT a + b * 2 FROM t WHERE x = 1 OR y = 2 AND z = 3");
        let SelectItem::Expr {
            expr: Expr::Binary { op, right, .. },
            ..
        } = &query.projection[0]
        else {
            panic!()
        };
        assert_eq!(*op, BinaryOp::Add);
        assert!(matches!(
            **right,
            Expr::Binary {
                op: BinaryOp::Mul,
                ..
            }
        ));
        assert!(matches!(
            query.selection,
            Some(Expr::Binary {
                op: BinaryOp::Or,
                ..
            })
        ));
    }

    #[test]
    fn parses_explain_physical() {
        assert!(matches!(
            parse_sql("EXPLAIN PHYSICAL SELECT * FROM t").unwrap(),
            Statement::Explain { physical: true, .. }
        ));
    }

    #[test]
    fn rejects_unsupported_statements_helpfully() {
        let error = parse_sql("INSERT INTO t VALUES (1)")
            .unwrap_err()
            .to_string();
        assert!(error.contains("DDL and mutation statements are not supported"));
    }

    #[test]
    fn rejects_fractional_limit() {
        assert!(parse_sql("SELECT * FROM t LIMIT 1.5").is_err());
    }
}
