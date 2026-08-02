//! 查询 DSL 解析器:将用户输入字符串编译为 tantivy Query。
//!
//! 支持的语法:
//! - 简单关键字: report 2024 (空格分隔, 默认 AND)
//! - 短语: "annual report"
//! - 扩展名: ext:pdf,doc
//! - 大小: size:>100MB, size:1KB-10MB
//! - 日期: dm:today, dm:>2024-01-01, dm:2024-01-01..2024-12-31
//! - 路径: path:Downloads
//! - 正则: regex:^Report-\d{4}
//! - 布尔: AND, OR, NOT, 括号分组

use chrono::{Datelike, NaiveDate};
use tantivy::schema::Schema;
use tantivy::query::{
    AllQuery, BooleanQuery, FuzzyTermQuery, Occur, Query, RangeQuery,
    RegexQuery, TermQuery,
};
use tantivy::Term;

use crate::search_index::schema::FieldId;

/// 查询 AST 节点。
#[derive(Debug, Clone, PartialEq)]
pub enum QueryNode {
    Empty,
    Keywords(Vec<String>),
    Phrase(String),
    Extension(Vec<String>),
    Size { op: CompareOp, value: u64 },
    SizeRange { min: u64, max: u64 },
    Date { op: CompareOp, value: DateValue },
    DateRange { start: DateValue, end: DateValue },
    Path(String),
    Regex(String),
    And(Box<QueryNode>, Box<QueryNode>),
    Or(Box<QueryNode>, Box<QueryNode>),
    Not(Box<QueryNode>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompareOp {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DateValue {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Absolute(NaiveDate),
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryParseError {
    Empty,
    InvalidSyntax(String),
    InvalidSize(String),
    InvalidDate(String),
    InvalidRegex(String),
}

impl std::fmt::Display for QueryParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "查询为空"),
            Self::InvalidSyntax(s) => write!(f, "语法错误: {s}"),
            Self::InvalidSize(s) => write!(f, "无效的大小值: {s}"),
            Self::InvalidDate(s) => write!(f, "无效的日期: {s}"),
            Self::InvalidRegex(s) => write!(f, "无效的正则表达式: {s}"),
        }
    }
}

impl std::error::Error for QueryParseError {}

/// 词法 token。
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Phrase(String),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            tokens.push(Token::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            tokens.push(Token::RParen);
            i += 1;
            continue;
        }
        if c == '"' {
            let mut end = i + 1;
            while end < chars.len() && chars[end] != '"' {
                end += 1;
            }
            if end >= chars.len() {
                return Err(QueryParseError::InvalidSyntax("未闭合的引号".to_owned()));
            }
            let phrase: String = chars[i + 1..end].iter().collect();
            tokens.push(Token::Phrase(phrase));
            i = end + 1;
            continue;
        }
        let mut end = i;
        while end < chars.len()
            && !chars[end].is_whitespace()
            && chars[end] != '('
            && chars[end] != ')'
        {
            end += 1;
        }
        let word: String = chars[i..end].iter().collect();
        match word.to_ascii_uppercase().as_str() {
            "AND" => tokens.push(Token::And),
            "OR" => tokens.push(Token::Or),
            "NOT" => tokens.push(Token::Not),
            _ => tokens.push(Token::Word(word)),
        }
        i = end;
    }
    Ok(tokens)
}

struct TokenStream {
    tokens: Vec<Token>,
    pos: usize,
}

impl TokenStream {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<QueryNode, QueryParseError> {
        let mut left = self.parse_and()?;
        while let Some(Token::Or) = self.peek() {
            self.next();
            let right = self.parse_and()?;
            left = QueryNode::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<QueryNode, QueryParseError> {
        let mut left = self.parse_not()?;
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.next();
                    let right = self.parse_not()?;
                    left = QueryNode::And(Box::new(left), Box::new(right));
                }
                Some(Token::Word(_)) | Some(Token::Phrase(_))
                | Some(Token::Not) | Some(Token::LParen) => {
                    let right = self.parse_not()?;
                    left = QueryNode::And(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<QueryNode, QueryParseError> {
        if let Some(Token::Not) = self.peek() {
            self.next();
            let inner = self.parse_not()?;
            return Ok(QueryNode::Not(Box::new(inner)));
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<QueryNode, QueryParseError> {
        match self.next() {
            Some(Token::LParen) => {
                let node = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(node),
                    _ => Err(QueryParseError::InvalidSyntax("缺少右括号".to_owned())),
                }
            }
            Some(Token::Phrase(s)) => Ok(QueryNode::Phrase(s)),
            Some(Token::Word(w)) => parse_field_or_keyword(&w),
            Some(t) => Err(QueryParseError::InvalidSyntax(format!("意外的 token: {t:?}"))),
            None => Err(QueryParseError::InvalidSyntax("意外的输入结束".to_owned())),
        }
    }
}

fn parse_field_or_keyword(word: &str) -> Result<QueryNode, QueryParseError> {
    let lower = word.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("ext:") {
        let exts: Vec<String> = rest
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if exts.is_empty() {
            return Err(QueryParseError::InvalidSyntax(
                "ext: 后需要扩展名".to_owned(),
            ));
        }
        return Ok(QueryNode::Extension(exts));
    }
    if let Some(rest) = lower.strip_prefix("size:") {
        return parse_size_field(rest);
    }
    if let Some(rest) = lower.strip_prefix("dm:") {
        return parse_date_field(rest);
    }
    if let Some(rest) = lower.strip_prefix("path:") {
        if rest.is_empty() {
            return Err(QueryParseError::InvalidSyntax(
                "path: 后需要路径".to_owned(),
            ));
        }
        return Ok(QueryNode::Path(rest.to_owned()));
    }
    Ok(QueryNode::Keywords(vec![word.to_owned()]))
}

fn parse_size_field(rest: &str) -> Result<QueryNode, QueryParseError> {
    if let Some(idx) = rest.find('-') {
        return Ok(QueryNode::SizeRange {
            min: parse_size(&rest[..idx])?,
            max: parse_size(&rest[idx + 1..])?,
        });
    }
    if let Some(val) = rest.strip_prefix(">=") {
        return Ok(QueryNode::Size {
            op: CompareOp::Gte,
            value: parse_size(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix("<=") {
        return Ok(QueryNode::Size {
            op: CompareOp::Lte,
            value: parse_size(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix('>') {
        return Ok(QueryNode::Size {
            op: CompareOp::Gt,
            value: parse_size(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix('<') {
        return Ok(QueryNode::Size {
            op: CompareOp::Lt,
            value: parse_size(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix('=') {
        return Ok(QueryNode::Size {
            op: CompareOp::Eq,
            value: parse_size(val)?,
        });
    }
    Ok(QueryNode::Size {
        op: CompareOp::Eq,
        value: parse_size(rest)?,
    })
}

fn parse_date_field(rest: &str) -> Result<QueryNode, QueryParseError> {
    if let Some(idx) = rest.find("..") {
        return Ok(QueryNode::DateRange {
            start: parse_date_value(&rest[..idx])?,
            end: parse_date_value(&rest[idx + 2..])?,
        });
    }
    if let Some(val) = rest.strip_prefix(">=") {
        return Ok(QueryNode::Date {
            op: CompareOp::Gte,
            value: parse_date_value(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix("<=") {
        return Ok(QueryNode::Date {
            op: CompareOp::Lte,
            value: parse_date_value(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix('>') {
        return Ok(QueryNode::Date {
            op: CompareOp::Gt,
            value: parse_date_value(val)?,
        });
    }
    if let Some(val) = rest.strip_prefix('<') {
        return Ok(QueryNode::Date {
            op: CompareOp::Lt,
            value: parse_date_value(val)?,
        });
    }
    Ok(QueryNode::Date {
        op: CompareOp::Eq,
        value: parse_date_value(rest)?,
    })
}

fn parse_date_value(s: &str) -> Result<DateValue, QueryParseError> {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "today" => Ok(DateValue::Today),
        "yesterday" => Ok(DateValue::Yesterday),
        "this-week" | "thisweek" => Ok(DateValue::ThisWeek),
        "this-month" | "thismonth" => Ok(DateValue::ThisMonth),
        _ => {
            let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| QueryParseError::InvalidDate(s.to_owned()))?;
            Ok(DateValue::Absolute(date))
        }
    }
}

/// 解析大小字符串为字节数。
pub fn parse_size(s: &str) -> Result<u64, QueryParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(QueryParseError::InvalidSize(s.to_owned()));
    }
    let (num_part, multiplier) = if let Some(rest) = s.strip_suffix("KB").or_else(|| s.strip_suffix("K")).or_else(|| s.strip_suffix("kb")).or_else(|| s.strip_suffix("k")) {
        (rest, 1024u64)
    } else if let Some(rest) = s.strip_suffix("MB").or_else(|| s.strip_suffix("M")).or_else(|| s.strip_suffix("mb")).or_else(|| s.strip_suffix("m")) {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s.strip_suffix("GB").or_else(|| s.strip_suffix("G")).or_else(|| s.strip_suffix("gb")).or_else(|| s.strip_suffix("g")) {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s.strip_suffix("TB").or_else(|| s.strip_suffix("T")).or_else(|| s.strip_suffix("tb")).or_else(|| s.strip_suffix("t")) {
        (rest, 1024u64 * 1024 * 1024 * 1024)
    } else {
        (s, 1u64)
    };
    let num: u64 = num_part.trim().parse().map_err(|_| {
        QueryParseError::InvalidSize(s.to_owned())
    })?;
    num.checked_mul(multiplier)
        .ok_or_else(|| QueryParseError::InvalidSize(s.to_owned()))
}

/// 解析查询字符串为 AST。
pub fn parse_query(input: &str) -> Result<QueryNode, QueryParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(QueryNode::Empty);
    }
    if let Some(rest) = trimmed.strip_prefix("regex:") {
        let pattern = rest.trim();
        if pattern.is_empty() {
            return Err(QueryParseError::InvalidRegex("空正则".to_owned()));
        }
        regex::Regex::new(pattern)
            .map_err(|e| QueryParseError::InvalidRegex(e.to_string()))?;
        return Ok(QueryNode::Regex(pattern.to_owned()));
    }
    let tokens = tokenize(trimmed)?;
    let mut parser = TokenStream::new(tokens);
    parser.parse_or()
}

/// 将 AST 编译为 tantivy Query 对象。
pub fn compile_query(
    node: &QueryNode,
    schema: &Schema,
) -> Result<Box<dyn Query>, QueryParseError> {
    match node {
        QueryNode::Empty => Ok(Box::new(AllQuery)),
        QueryNode::Keywords(keywords) => {
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for kw in keywords {
                let term_name = Term::from_field_text(FieldId::name(schema), kw);
                let term_path = Term::from_field_text(FieldId::path(schema), kw);
                let or_query = BooleanQuery::new(vec![
                    (Occur::Should, Box::new(FuzzyTermQuery::new(term_name, 0, true)) as Box<dyn Query>),
                    (Occur::Should, Box::new(FuzzyTermQuery::new(term_path, 0, true)) as Box<dyn Query>),
                ]);
                clauses.push((Occur::Must, Box::new(or_query)));
            }
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
        QueryNode::Phrase(s) => {
            let term = Term::from_field_text(FieldId::name(schema), s);
            Ok(Box::new(TermQuery::new(term, Default::default())))
        }
        QueryNode::Extension(exts) => {
            let clauses: Vec<(Occur, Box<dyn Query>)> = exts.iter().map(|e| {
                let term = Term::from_field_text(FieldId::extension(schema), e);
                (Occur::Should, Box::new(TermQuery::new(term, Default::default())) as Box<dyn Query>)
            }).collect();
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
        QueryNode::Size { op, value } => {
            let field = FieldId::size(schema);
            let field_name = schema.get_field_name(field).to_string();
            let bounds: (std::ops::Bound<u64>, std::ops::Bound<u64>) = match op {
                CompareOp::Gt => (std::ops::Bound::Excluded(*value), std::ops::Bound::Unbounded),
                CompareOp::Lt => (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(*value)),
                CompareOp::Gte => (std::ops::Bound::Included(*value), std::ops::Bound::Unbounded),
                CompareOp::Lte => (std::ops::Bound::Unbounded, std::ops::Bound::Included(*value)),
                CompareOp::Eq => (std::ops::Bound::Included(*value), std::ops::Bound::Included(*value)),
            };
            Ok(Box::new(RangeQuery::new_u64_bounds(
                field_name,
                bounds.0,
                bounds.1,
            )))
        }
        QueryNode::SizeRange { min, max } => {
            let field = FieldId::size(schema);
            let field_name = schema.get_field_name(field).to_string();
            Ok(Box::new(RangeQuery::new_u64_bounds(
                field_name,
                std::ops::Bound::Included(*min),
                std::ops::Bound::Included(*max),
            )))
        }
        QueryNode::Date { op, value } => {
            let field = FieldId::modified_days(schema);
            let field_name = schema.get_field_name(field).to_string();
            let day = date_value_to_days(value);
            let bounds: (std::ops::Bound<u64>, std::ops::Bound<u64>) = match op {
                CompareOp::Gt => (std::ops::Bound::Excluded(day), std::ops::Bound::Unbounded),
                CompareOp::Lt => (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(day)),
                CompareOp::Gte => (std::ops::Bound::Included(day), std::ops::Bound::Unbounded),
                CompareOp::Lte => (std::ops::Bound::Unbounded, std::ops::Bound::Included(day)),
                CompareOp::Eq => (std::ops::Bound::Included(day), std::ops::Bound::Included(day)),
            };
            Ok(Box::new(RangeQuery::new_u64_bounds(
                field_name,
                bounds.0,
                bounds.1,
            )))
        }
        QueryNode::DateRange { start, end } => {
            let field = FieldId::modified_days(schema);
            let field_name = schema.get_field_name(field).to_string();
            let start_day = date_value_to_days(start);
            let end_day = date_value_to_days(end);
            Ok(Box::new(RangeQuery::new_u64_bounds(
                field_name,
                std::ops::Bound::Included(start_day),
                std::ops::Bound::Included(end_day),
            )))
        }
        QueryNode::Path(s) => {
            let term = Term::from_field_text(FieldId::path(schema), s);
            Ok(Box::new(TermQuery::new(term, Default::default())))
        }
        QueryNode::Regex(pattern) => {
            Ok(Box::new(RegexQuery::from_pattern(pattern, FieldId::name(schema))
                .map_err(|e| QueryParseError::InvalidRegex(e.to_string()))?))
        }
        QueryNode::And(left, right) => {
            let l = compile_query(left, schema)?;
            let r = compile_query(right, schema)?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, l),
                (Occur::Must, r),
            ])))
        }
        QueryNode::Or(left, right) => {
            let l = compile_query(left, schema)?;
            let r = compile_query(right, schema)?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Should, l),
                (Occur::Should, r),
            ])))
        }
        QueryNode::Not(inner) => {
            let sub = compile_query(inner, schema)?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery)),
                (Occur::MustNot, sub),
            ])))
        }
    }
}

fn date_value_to_days(value: &DateValue) -> u64 {
    let today = chrono::Local::now().date_naive();
    let date = match value {
        DateValue::Today => today,
        DateValue::Yesterday => today - chrono::Duration::days(1),
        DateValue::ThisWeek => today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64),
        DateValue::ThisMonth => chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today),
        DateValue::Absolute(d) => *d,
    };
    (date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() / 86400) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::Schema;
    use crate::search_index::schema::create_schema;

    fn test_schema() -> Schema {
        create_schema()
    }

    #[test]
    fn empty_input_returns_empty_node() {
        assert_eq!(parse_query("").unwrap(), QueryNode::Empty);
        assert_eq!(parse_query("   ").unwrap(), QueryNode::Empty);
    }

    #[test]
    fn single_keyword() {
        assert_eq!(
            parse_query("report").unwrap(),
            QueryNode::Keywords(vec!["report".to_owned()]),
        );
    }

    #[test]
    fn multiple_keywords() {
        // 多个关键字用 AND 组合
        let result = parse_query("report 2024 pdf").unwrap();
        // 收集所有 And 节点叶子
        let mut keywords = Vec::new();
        collect_keywords(&result, &mut keywords);
        assert_eq!(keywords, vec!["report", "2024", "pdf"]);
    }

    fn collect_keywords(node: &QueryNode, out: &mut Vec<String>) {
        match node {
            QueryNode::Keywords(ks) => {
                for k in ks {
                    out.push(k.clone());
                }
            }
            QueryNode::And(l, r) => {
                collect_keywords(l, out);
                collect_keywords(r, out);
            }
            _ => {}
        }
    }

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
    }

    #[test]
    fn parse_size_with_suffix() {
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("100MB").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024u64 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_invalid() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn ext_field_single() {
        assert_eq!(
            parse_query("ext:pdf").unwrap(),
            QueryNode::Extension(vec!["pdf".to_owned()]),
        );
    }

    #[test]
    fn ext_field_multiple() {
        assert_eq!(
            parse_query("ext:pdf,doc,xlsx").unwrap(),
            QueryNode::Extension(vec![
                "pdf".to_owned(),
                "doc".to_owned(),
                "xlsx".to_owned(),
            ]),
        );
    }

    #[test]
    fn size_field_greater_than() {
        assert_eq!(
            parse_query("size:>100MB").unwrap(),
            QueryNode::Size {
                op: CompareOp::Gt,
                value: 100 * 1024 * 1024,
            },
        );
    }

    #[test]
    fn size_field_range() {
        assert_eq!(
            parse_query("size:1KB-10MB").unwrap(),
            QueryNode::SizeRange {
                min: 1024,
                max: 10 * 1024 * 1024,
            },
        );
    }

    #[test]
    fn dm_field_today() {
        assert_eq!(
            parse_query("dm:today").unwrap(),
            QueryNode::Date {
                op: CompareOp::Eq,
                value: DateValue::Today,
            },
        );
    }

    #[test]
    fn dm_field_absolute_date() {
        assert_eq!(
            parse_query("dm:>2024-01-01").unwrap(),
            QueryNode::Date {
                op: CompareOp::Gt,
                value: DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            },
        );
    }

    #[test]
    fn dm_field_date_range() {
        let result = parse_query("dm:2024-01-01..2024-12-31").unwrap();
        match result {
            QueryNode::DateRange { start, end } => {
                assert_eq!(
                    start,
                    DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
                );
                assert_eq!(
                    end,
                    DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
                );
            }
            other => panic!("期望 DateRange, 实际: {other:?}"),
        }
    }

    #[test]
    fn path_field() {
        // 字段值被小写化 (大小写不敏感搜索)
        assert_eq!(
            parse_query("path:Downloads").unwrap(),
            QueryNode::Path("downloads".to_owned()),
        );
    }

    #[test]
    fn regex_prefix() {
        assert_eq!(
            parse_query("regex:^Report-\\d{4}").unwrap(),
            QueryNode::Regex("^Report-\\d{4}".to_owned()),
        );
    }

    #[test]
    fn invalid_regex() {
        assert!(parse_query("regex:[invalid").is_err());
    }

    #[test]
    fn phrase_with_quotes() {
        assert_eq!(
            parse_query("\"annual report\"").unwrap(),
            QueryNode::Phrase("annual report".to_owned()),
        );
    }

    #[test]
    fn boolean_and() {
        let result = parse_query("report AND pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(
                    *left,
                    QueryNode::Keywords(vec!["report".to_owned()])
                );
                assert_eq!(*right, QueryNode::Keywords(vec!["pdf".to_owned()]));
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }

    #[test]
    fn boolean_or() {
        let result = parse_query("report OR summary").unwrap();
        match result {
            QueryNode::Or(left, right) => {
                assert_eq!(
                    *left,
                    QueryNode::Keywords(vec!["report".to_owned()])
                );
                assert_eq!(
                    *right,
                    QueryNode::Keywords(vec!["summary".to_owned()])
                );
            }
            other => panic!("期望 Or, 实际: {other:?}"),
        }
    }

    #[test]
    fn boolean_not() {
        let result = parse_query("NOT tmp").unwrap();
        match result {
            QueryNode::Not(inner) => {
                assert_eq!(*inner, QueryNode::Keywords(vec!["tmp".to_owned()]));
            }
            other => panic!("期望 Not, 实际: {other:?}"),
        }
    }

    #[test]
    fn parentheses_group() {
        let result = parse_query("(report OR summary) AND pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(*right, QueryNode::Keywords(vec!["pdf".to_owned()]));
                match *left {
                    QueryNode::Or(l, r) => {
                        assert_eq!(*l, QueryNode::Keywords(vec!["report".to_owned()]));
                        assert_eq!(*r, QueryNode::Keywords(vec!["summary".to_owned()]));
                    }
                    other => panic!("左子节点应为 Or, 实际: {other:?}"),
                }
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }

    #[test]
    fn combined_keyword_and_field() {
        let result = parse_query("report ext:pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(*left, QueryNode::Keywords(vec!["report".to_owned()]));
                assert_eq!(
                    *right,
                    QueryNode::Extension(vec!["pdf".to_owned()])
                );
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }

    #[test]
    fn compile_empty_query() {
        let schema = test_schema();
        let node = parse_query("").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("All"));
    }

    #[test]
    fn compile_extension_query() {
        let schema = test_schema();
        let node = parse_query("ext:pdf").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Term") || format!("{query:?}").contains("Boolean"));
    }

    #[test]
    fn compile_size_range_query() {
        let schema = test_schema();
        let node = parse_query("size:1KB-10MB").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Range"));
    }

    #[test]
    fn compile_regex_query() {
        let schema = test_schema();
        let node = parse_query("regex:Report").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Regex"));
    }

    #[test]
    fn compile_boolean_not() {
        let schema = test_schema();
        let node = parse_query("NOT tmp").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Boolean"));
    }
}
