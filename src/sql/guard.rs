//! AST allow-list for statements sent to a source database.
//!
//! ```compile_fail
//! let _g = dipper::sql::guard::GuardedSelect {
//!     sql: String::new(),
//!     dialect: dipper::sql::guard::SqlDialect::Sqlite,
//! };
//! ```
//!
//! ```compile_fail
//! let _g = dipper::sql::guard::GuardedSelect::from(String::new());
//! ```

use std::ops::ControlFlow;

use sqlparser::ast::visit_expressions;
use sqlparser::ast::{
    BinaryOperator, Expr, Fetch, Function, FunctionArg, FunctionArgExpr, FunctionArguments,
    GroupByExpr, Ident, LimitClause, ObjectName, OrderBy, OrderByKind, Query, Select, SelectFlavor,
    SelectItem, SetExpr, Statement, TableFactor, Value, ValueWithSpan,
};
use sqlparser::dialect::{Dialect, OracleDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

/// Which parser dialect to use when checking a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Oracle,
    Sqlite,
}

/// One SELECT that passed the allow-list. The only constructor is [`GuardedSelect::parse`].
#[derive(Debug, Clone)]
pub struct GuardedSelect {
    sql: String,
    dialect: SqlDialect,
}

impl GuardedSelect {
    /// Parse `sql` and accept it only when it is one simple read-only SELECT.
    pub fn parse(sql: &str, dialect: SqlDialect) -> Result<Self, GuardError> {
        reject_comments(sql, dialect)?;
        let parsed = Parser::parse_sql(dialect_impl(dialect).as_ref(), sql)
            .map_err(|err| GuardError(err.to_string()))?;
        if parsed.len() != 1 {
            return Err(GuardError("expected exactly one SQL statement".to_string()));
        }
        let Statement::Query(query) = &parsed[0] else {
            return Err(GuardError("only a single SELECT is allowed".to_string()));
        };
        check_query(query)?;
        if let ControlFlow::Break(err) = visit_expressions(query.as_ref(), |expr| {
            if let Expr::Function(function) = expr {
                if !is_count_star(function) {
                    return ControlFlow::Break(GuardError("only COUNT(*) is allowed".to_string()));
                }
            }
            ControlFlow::Continue(())
        }) {
            return Err(err);
        }
        Ok(Self {
            sql: sql.to_string(),
            dialect,
        })
    }

    pub fn sql(&self) -> &str {
        &self.sql
    }

    pub fn dialect(&self) -> SqlDialect {
        self.dialect
    }
}

/// Why a statement was not a permitted SELECT.
#[derive(Debug, thiserror::Error)]
#[error("rejected SQL: {0}")]
pub struct GuardError(pub String);

fn dialect_impl(dialect: SqlDialect) -> Box<dyn Dialect> {
    match dialect {
        SqlDialect::Oracle => Box::new(OracleDialect),
        SqlDialect::Sqlite => Box::new(SQLiteDialect {}),
    }
}

fn reject_comments(sql: &str, dialect: SqlDialect) -> Result<(), GuardError> {
    let dialect = dialect_impl(dialect);
    let mut tokenizer = Tokenizer::new(dialect.as_ref(), sql);
    let tokens = tokenizer
        .tokenize()
        .map_err(|err| GuardError(err.to_string()))?;
    for token in tokens {
        if let Token::Whitespace(
            Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_),
        ) = token
        {
            return Err(GuardError("comments are not allowed".to_string()));
        }
    }
    Ok(())
}

fn check_query(query: &Query) -> Result<(), GuardError> {
    if query.with.is_some() {
        return Err(GuardError("WITH is not allowed".to_string()));
    }
    if query.for_clause.is_some() || query.settings.is_some() || query.format_clause.is_some() {
        return Err(GuardError("extra query clause".to_string()));
    }
    if !query.locks.is_empty() || !query.pipe_operators.is_empty() {
        return Err(GuardError("locks are not allowed".to_string()));
    }
    if let Some(order_by) = &query.order_by {
        check_order_by(order_by)?;
    }
    if let Some(fetch) = &query.fetch {
        check_fetch(fetch)?;
    }
    if let Some(limit) = &query.limit_clause {
        check_limit(limit)?;
    }
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(GuardError("compound queries are not allowed".to_string()));
    };
    check_select(select)
}

fn check_order_by(order_by: &OrderBy) -> Result<(), GuardError> {
    if order_by.interpolate.is_some() {
        return Err(GuardError("ORDER BY must be an identifier".to_string()));
    }
    let OrderByKind::Expressions(exprs) = &order_by.kind else {
        return Err(GuardError("ORDER BY must be an identifier".to_string()));
    };
    if exprs.is_empty() {
        return Err(GuardError("ORDER BY must be an identifier".to_string()));
    }
    for expr in exprs {
        if expr.options.sort.is_some()
            || expr.options.nulls_first.is_some()
            || expr.with_fill.is_some()
        {
            return Err(GuardError("ORDER BY must be an identifier".to_string()));
        }
        match &expr.expr {
            Expr::Identifier(ident) => check_ident(ident)?,
            Expr::CompoundIdentifier(idents) => idents.iter().try_for_each(check_ident)?,
            _ => return Err(GuardError("ORDER BY must be an identifier".to_string())),
        }
    }
    Ok(())
}

fn check_fetch(fetch: &Fetch) -> Result<(), GuardError> {
    if fetch.with_ties || fetch.percent {
        return Err(GuardError(
            "only FETCH FIRST n ROWS ONLY is allowed".to_string(),
        ));
    }
    let Some(quantity) = &fetch.quantity else {
        return Err(GuardError("FETCH requires a row count".to_string()));
    };
    if !is_limit_expr(quantity) {
        return Err(GuardError(
            "FETCH row count must be a placeholder or number".to_string(),
        ));
    }
    Ok(())
}

fn check_limit(limit: &LimitClause) -> Result<(), GuardError> {
    let LimitClause::LimitOffset {
        limit,
        offset: None,
        limit_by,
    } = limit
    else {
        return Err(GuardError("OFFSET is not allowed".to_string()));
    };
    if !limit_by.is_empty() {
        return Err(GuardError("LIMIT BY is not allowed".to_string()));
    }
    let Some(limit) = limit else {
        return Err(GuardError("LIMIT requires a row count".to_string()));
    };
    if !is_limit_expr(limit) {
        return Err(GuardError(
            "LIMIT must be a placeholder or number".to_string(),
        ));
    }
    Ok(())
}

fn check_select(select: &Select) -> Result<(), GuardError> {
    if select.flavor != SelectFlavor::Standard {
        return Err(GuardError("only a standard SELECT is allowed".to_string()));
    }
    if !select.optimizer_hints.is_empty()
        || select.distinct.is_some()
        || select.select_modifiers.is_some()
        || select.top.is_some()
        || select.exclude.is_some()
        || select.into.is_some()
        || select.prewhere.is_some()
        || select.having.is_some()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
    {
        return Err(GuardError("unsupported SELECT clause".to_string()));
    }
    if !select.lateral_views.is_empty()
        || !select.connect_by.is_empty()
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || !select.named_window.is_empty()
    {
        return Err(GuardError("unsupported SELECT clause".to_string()));
    }
    if !matches!(
        &select.group_by,
        GroupByExpr::Expressions(exprs, mods) if exprs.is_empty() && mods.is_empty()
    ) {
        return Err(GuardError("GROUP BY is not allowed".to_string()));
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(GuardError("exactly one table is allowed".to_string()));
    }
    check_table(&select.from[0].relation)?;
    if select.projection.is_empty() {
        return Err(GuardError("SELECT list is empty".to_string()));
    }
    for item in &select.projection {
        check_projection(item)?;
    }
    if let Some(selection) = &select.selection {
        check_where(selection)?;
    }
    Ok(())
}

fn check_table(relation: &TableFactor) -> Result<(), GuardError> {
    let TableFactor::Table {
        name,
        alias: None,
        args: None,
        with_hints,
        version: None,
        with_ordinality: false,
        partitions,
        json_path: None,
        sample: None,
        index_hints,
    } = relation
    else {
        return Err(GuardError("only a plain table name is allowed".to_string()));
    };
    if !with_hints.is_empty() || !partitions.is_empty() || !index_hints.is_empty() {
        return Err(GuardError("table hints are not allowed".to_string()));
    }
    check_object_name(name)
}

fn check_projection(item: &SelectItem) -> Result<(), GuardError> {
    let SelectItem::UnnamedExpr(expr) = item else {
        return Err(GuardError("SELECT aliases are not allowed".to_string()));
    };
    match expr {
        Expr::Identifier(ident) => check_ident(ident),
        Expr::CompoundIdentifier(idents) => idents.iter().try_for_each(check_ident),
        Expr::Value(value) if is_number(value) => Ok(()),
        Expr::Function(function) if is_count_star(function) => Ok(()),
        _ => Err(GuardError(
            "SELECT list must be a column, number, or COUNT(*)".to_string(),
        )),
    }
}

fn check_where(expr: &Expr) -> Result<(), GuardError> {
    match expr {
        Expr::Identifier(ident) => check_ident(ident),
        Expr::CompoundIdentifier(idents) => idents.iter().try_for_each(check_ident),
        Expr::Nested(inner) => check_where(inner),
        Expr::Value(value) if is_number(value) || is_placeholder(value) => Ok(()),
        Expr::BinaryOp { left, op, right } if is_allowed_operator(op) => {
            check_where(left)?;
            check_where(right)
        }
        Expr::Like {
            negated: false,
            any: false,
            expr,
            pattern,
            escape_char: None,
        } => {
            check_where(expr)?;
            check_where(pattern)
        }
        Expr::Between {
            expr,
            negated: false,
            low,
            high,
        } => {
            check_where(expr)?;
            check_where(low)?;
            check_where(high)
        }
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            check_where(expr)?;
            for item in list {
                check_where(item)?;
            }
            Ok(())
        }
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => check_where(inner),
        _ => Err(GuardError("unsupported WHERE expression".to_string())),
    }
}

fn is_allowed_operator(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::And
            | BinaryOperator::Or
            | BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
    )
}

fn is_limit_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Value(value) => is_number(value) || is_placeholder(value),
        _ => false,
    }
}

fn is_number(value: &ValueWithSpan) -> bool {
    matches!(value.value, Value::Number(_, _))
}

fn is_placeholder(value: &ValueWithSpan) -> bool {
    matches!(value.value, Value::Placeholder(_))
}

fn is_count_star(function: &Function) -> bool {
    if function.uses_odbc_syntax
        || function.filter.is_some()
        || function.over.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
    {
        return false;
    }
    let FunctionArguments::List(list) = &function.args else {
        return false;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() || list.args.len() != 1 {
        return false;
    }
    if !matches!(
        list.args[0],
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard)
    ) {
        return false;
    }
    let Some(ident) = function.name.0.first().and_then(|part| part.as_ident()) else {
        return false;
    };
    function.name.0.len() == 1
        && ident.value.eq_ignore_ascii_case("count")
        && check_ident(ident).is_ok()
}

fn check_object_name(name: &ObjectName) -> Result<(), GuardError> {
    if name.0.is_empty() {
        return Err(GuardError("missing table name".to_string()));
    }
    for part in &name.0 {
        let Some(ident) = part.as_ident() else {
            return Err(GuardError("table name must be an identifier".to_string()));
        };
        check_ident(ident)?;
    }
    Ok(())
}

fn check_ident(ident: &Ident) -> Result<(), GuardError> {
    if ident.value.contains('@') {
        return Err(GuardError("database links are not allowed".to_string()));
    }
    Ok(())
}
