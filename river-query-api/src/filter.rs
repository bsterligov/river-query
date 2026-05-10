/// Filter DSL: `key:value`, `key:>value`, `key:>=value`, `key:<value`, `key:<=value`,
/// `AND`, `OR`, `NOT`, parentheses, wildcard `*` suffix.
///
/// Grammar:
///   expr     := or_expr
///   or_expr  := and_expr (OR and_expr)*
///   and_expr := not_expr (AND not_expr)*
///   not_expr := NOT not_expr | primary
///   primary  := FILTER | LPAREN expr RPAREN

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Debug, PartialEq)]
pub enum Expr {
    Cmp { key: String, op: Op, value: String },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, PartialEq)]
enum Tok {
    And,
    Or,
    Not,
    LParen,
    RParen,
    Filter(String, Op, String),
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            ' ' | '\t' => {
                chars.next();
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ' ' || c == '\t' || c == '(' || c == ')' {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                toks.push(classify_word(&word)?);
            }
        }
    }
    Ok(toks)
}

fn classify_word(word: &str) -> Result<Tok, String> {
    match word {
        "AND" => return Ok(Tok::And),
        "OR" => return Ok(Tok::Or),
        "NOT" => return Ok(Tok::Not),
        _ => {}
    }
    if let Some(colon) = word.find(':') {
        let key = word[..colon].to_string();
        if key.is_empty() {
            return Err(format!("empty key in filter: {word}"));
        }
        let rest = &word[colon + 1..];
        let (op, value) = parse_op_value(rest)?;
        Ok(Tok::Filter(key, op, value))
    } else {
        Err(format!("unexpected token: {word}"))
    }
}

fn parse_op_value(s: &str) -> Result<(Op, String), String> {
    if let Some(rest) = s.strip_prefix(">=") {
        Ok((Op::Gte, rest.to_string()))
    } else if let Some(rest) = s.strip_prefix("<=") {
        Ok((Op::Lte, rest.to_string()))
    } else if let Some(rest) = s.strip_prefix('>') {
        Ok((Op::Gt, rest.to_string()))
    } else if let Some(rest) = s.strip_prefix('<') {
        Ok((Op::Lt, rest.to_string()))
    } else {
        Ok((Op::Eq, s.to_string()))
    }
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Tok>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&Tok> {
        let tok = self.tokens.get(self.pos);
        self.pos += 1;
        tok
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Tok::Or) {
            self.next();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_not()?;
        while self.peek() == Some(&Tok::And) {
            self.next();
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.next();
                let expr = self.parse_expr()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err("expected closing ')'".to_string());
                }
                self.next();
                Ok(expr)
            }
            Some(Tok::Filter(_, _, _)) => {
                if let Some(Tok::Filter(key, op, value)) = self.next() {
                    Ok(Expr::Cmp {
                        key: key.clone(),
                        op: op.clone(),
                        value: value.clone(),
                    })
                } else {
                    Err("internal error".to_string())
                }
            }
            Some(tok) => Err(format!("unexpected token: {tok:?}")),
            None => Err("unexpected end of filter expression".to_string()),
        }
    }
}

pub fn parse(input: &str) -> Result<Option<Expr>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let toks = tokenize(input)?;
    let mut parser = Parser::new(toks);
    let expr = parser.parse_expr()?;
    if parser.pos < parser.tokens.len() {
        return Err(format!(
            "unexpected token at position {}: {:?}",
            parser.pos, parser.tokens[parser.pos]
        ));
    }
    Ok(Some(expr))
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "\\'")
}

fn clickhouse_field_logs(key: &str) -> &str {
    match key {
        "service" => "service_name",
        "level" | "severity" => "severity_text",
        "body" => "body",
        "trace_id" => "trace_id",
        "span_id" => "span_id",
        other => other,
    }
}

fn clickhouse_field_traces(key: &str) -> &str {
    match key {
        "service" => "service_name",
        "operation" => "operation_name",
        "trace_id" => "trace_id",
        "span_id" => "span_id",
        "status" => "status_code",
        other => other,
    }
}

fn cmp_to_ch(
    key: &str,
    op: &Op,
    value: &str,
    field_map: fn(&str) -> &str,
) -> Result<String, String> {
    let field = field_map(key);

    // duration_ms needs conversion to ns
    if key == "duration_ms" {
        let ms: u64 = value
            .parse()
            .map_err(|_| format!("duration_ms value must be numeric: {value}"))?;
        let ns = ms * 1_000_000;
        let op_str = match op {
            Op::Eq => "=",
            Op::Gt => ">",
            Op::Lt => "<",
            Op::Gte => ">=",
            Op::Lte => "<=",
        };
        return Ok(format!("duration_ns {op_str} {ns}"));
    }

    let has_wildcard = value.contains('*');
    match op {
        Op::Eq if has_wildcard => {
            let pattern = escape_sql(&value.replace('*', "%"));
            Ok(format!("{field} LIKE '{pattern}'"))
        }
        Op::Eq => Ok(format!("{field} = '{}'", escape_sql(value))),
        Op::Gt => Ok(format!("{field} > '{}'", escape_sql(value))),
        Op::Lt => Ok(format!("{field} < '{}'", escape_sql(value))),
        Op::Gte => Ok(format!("{field} >= '{}'", escape_sql(value))),
        Op::Lte => Ok(format!("{field} <= '{}'", escape_sql(value))),
    }
}

fn expr_to_ch(expr: &Expr, field_map: fn(&str) -> &str) -> Result<String, String> {
    match expr {
        Expr::Cmp { key, op, value } => cmp_to_ch(key, op, value, field_map),
        Expr::And(l, r) => Ok(format!(
            "({} AND {})",
            expr_to_ch(l, field_map)?,
            expr_to_ch(r, field_map)?
        )),
        Expr::Or(l, r) => Ok(format!(
            "({} OR {})",
            expr_to_ch(l, field_map)?,
            expr_to_ch(r, field_map)?
        )),
        Expr::Not(inner) => Ok(format!("NOT ({})", expr_to_ch(inner, field_map)?)),
    }
}

pub fn to_clickhouse_logs(expr: &Expr) -> Result<String, String> {
    expr_to_ch(expr, clickhouse_field_logs)
}

pub fn to_clickhouse_traces(expr: &Expr) -> Result<String, String> {
    expr_to_ch(expr, clickhouse_field_traces)
}

fn cmp_to_vm(key: &str, op: &Op, value: &str) -> Result<String, String> {
    if !matches!(op, Op::Eq) {
        return Err(format!(
            "VictoriaMetrics label selectors only support '=' equality, got key={key}"
        ));
    }
    let label = if key == "name" { "__name__" } else { key };
    if value.contains('*') {
        let pattern = value.replace('*', ".*");
        Ok(format!("{label}=~\"{pattern}\""))
    } else {
        Ok(format!("{label}=\"{value}\""))
    }
}

fn expr_to_vm(expr: &Expr) -> Result<Vec<String>, String> {
    match expr {
        Expr::Cmp { key, op, value } => Ok(vec![cmp_to_vm(key, op, value)?]),
        Expr::And(l, r) => {
            let mut parts = expr_to_vm(l)?;
            parts.extend(expr_to_vm(r)?);
            Ok(parts)
        }
        Expr::Or(_, _) => Err("OR is not supported in metric filter selectors".to_string()),
        Expr::Not(_) => Err("NOT is not supported in metric filter selectors".to_string()),
    }
}

pub fn to_vm_selector(expr: &Expr) -> Result<String, String> {
    let parts = expr_to_vm(expr)?;
    Ok(format!("{{{}}}", parts.join(", ")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_returns_none_for_empty() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("   ").unwrap(), None);
    }

    #[test]
    fn parse_single_equality() {
        let expr = parse("service:myapp").unwrap().unwrap();
        assert_eq!(
            expr,
            Expr::Cmp {
                key: "service".into(),
                op: Op::Eq,
                value: "myapp".into()
            }
        );
    }

    #[test]
    fn parse_comparison_ops() {
        let gt = parse("duration_ms:>500").unwrap().unwrap();
        assert!(matches!(gt, Expr::Cmp { op: Op::Gt, .. }));

        let lt = parse("duration_ms:<100").unwrap().unwrap();
        assert!(matches!(lt, Expr::Cmp { op: Op::Lt, .. }));

        let gte = parse("duration_ms:>=500").unwrap().unwrap();
        assert!(matches!(gte, Expr::Cmp { op: Op::Gte, .. }));

        let lte = parse("duration_ms:<=100").unwrap().unwrap();
        assert!(matches!(lte, Expr::Cmp { op: Op::Lte, .. }));
    }

    #[test]
    fn parse_and_expression() {
        let expr = parse("service:myapp AND level:error").unwrap().unwrap();
        assert!(matches!(expr, Expr::And(_, _)));
    }

    #[test]
    fn parse_or_expression() {
        let expr = parse("service:a OR service:b").unwrap().unwrap();
        assert!(matches!(expr, Expr::Or(_, _)));
    }

    #[test]
    fn parse_not_expression() {
        let expr = parse("NOT service:myapp").unwrap().unwrap();
        assert!(matches!(expr, Expr::Not(_)));
    }

    #[test]
    fn parse_precedence_and_over_or() {
        // a OR b AND c  →  a OR (b AND c)
        let expr = parse("service:a OR service:b AND service:c")
            .unwrap()
            .unwrap();
        match expr {
            Expr::Or(left, right) => {
                assert!(matches!(*left, Expr::Cmp { .. }));
                assert!(matches!(*right, Expr::And(_, _)));
            }
            _ => panic!("expected Or at top level"),
        }
    }

    #[test]
    fn parse_parens_override_precedence() {
        // (a OR b) AND c
        let expr = parse("(service:a OR service:b) AND service:c")
            .unwrap()
            .unwrap();
        assert!(matches!(expr, Expr::And(_, _)));
    }

    #[test]
    fn parse_wildcard_value() {
        let expr = parse("service:demo*").unwrap().unwrap();
        match expr {
            Expr::Cmp { value, .. } => assert_eq!(value, "demo*"),
            _ => panic!("expected Cmp"),
        }
    }

    #[test]
    fn parse_rejects_unknown_token() {
        assert!(parse("foobar").is_err());
    }

    #[test]
    fn parse_rejects_empty_key() {
        assert!(parse(":value").is_err());
    }

    #[test]
    fn parse_rejects_trailing_garbage() {
        assert!(parse("service:myapp AND").is_err());
    }

    #[test]
    fn logs_equality_maps_service() {
        let expr = parse("service:myapp").unwrap().unwrap();
        assert_eq!(to_clickhouse_logs(&expr).unwrap(), "service_name = 'myapp'");
    }

    #[test]
    fn logs_equality_maps_level() {
        let expr = parse("level:error").unwrap().unwrap();
        assert_eq!(
            to_clickhouse_logs(&expr).unwrap(),
            "severity_text = 'error'"
        );
    }

    #[test]
    fn logs_and_filter() {
        let expr = parse("service:myapp AND level:error").unwrap().unwrap();
        assert_eq!(
            to_clickhouse_logs(&expr).unwrap(),
            "(service_name = 'myapp' AND severity_text = 'error')"
        );
    }

    #[test]
    fn traces_duration_ms_converts_to_ns() {
        let expr = parse("duration_ms:>500").unwrap().unwrap();
        assert_eq!(
            to_clickhouse_traces(&expr).unwrap(),
            "duration_ns > 500000000"
        );
    }

    #[test]
    fn logs_wildcard_uses_like() {
        let expr = parse("service:demo*").unwrap().unwrap();
        assert_eq!(
            to_clickhouse_logs(&expr).unwrap(),
            "service_name LIKE 'demo%'"
        );
    }

    #[test]
    fn vm_selector_equality() {
        let expr = parse("name:http_requests_total AND service:myapp")
            .unwrap()
            .unwrap();
        let sel = to_vm_selector(&expr).unwrap();
        assert!(sel.contains("__name__=\"http_requests_total\""));
        assert!(sel.contains("service=\"myapp\""));
    }

    #[test]
    fn vm_selector_wildcard_uses_regex() {
        let expr = parse("name:http_requests*").unwrap().unwrap();
        let sel = to_vm_selector(&expr).unwrap();
        assert_eq!(sel, "{__name__=~\"http_requests.*\"}");
    }

    #[test]
    fn vm_selector_rejects_or() {
        let expr = parse("service:a OR service:b").unwrap().unwrap();
        assert!(to_vm_selector(&expr).is_err());
    }

    #[test]
    fn sql_escapes_single_quotes() {
        let expr = parse("body:it's").unwrap().unwrap();
        let sql = to_clickhouse_logs(&expr).unwrap();
        assert!(sql.contains("\\'"));
    }
}
