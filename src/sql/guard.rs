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
        check_query(query, dialect)?;
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

fn check_query(query: &Query, dialect: SqlDialect) -> Result<(), GuardError> {
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
        check_order_by(order_by, dialect)?;
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
    check_select(select, dialect)
}

fn check_order_by(order_by: &OrderBy, dialect: SqlDialect) -> Result<(), GuardError> {
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
            Expr::Identifier(ident) => check_ident(ident, dialect)?,
            Expr::CompoundIdentifier(_) => {
                return Err(GuardError(
                    "qualified column names are not allowed".to_string(),
                ));
            }
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

fn check_select(select: &Select, dialect: SqlDialect) -> Result<(), GuardError> {
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
    check_table(&select.from[0].relation, dialect)?;
    if select.projection.is_empty() {
        return Err(GuardError("SELECT list is empty".to_string()));
    }
    for item in &select.projection {
        check_projection(item, dialect)?;
    }
    if let Some(selection) = &select.selection {
        check_where(selection, dialect)?;
    }
    Ok(())
}

fn check_table(relation: &TableFactor, dialect: SqlDialect) -> Result<(), GuardError> {
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
    check_object_name(name, dialect)
}

fn check_projection(item: &SelectItem, dialect: SqlDialect) -> Result<(), GuardError> {
    let SelectItem::UnnamedExpr(expr) = item else {
        return Err(GuardError("SELECT aliases are not allowed".to_string()));
    };
    match expr {
        Expr::Identifier(ident) => check_ident(ident, dialect),
        Expr::CompoundIdentifier(_) => Err(GuardError(
            "qualified column names are not allowed".to_string(),
        )),
        Expr::Value(value) if is_number(value) => Ok(()),
        Expr::Function(function) if is_count_star(function) => Ok(()),
        _ => Err(GuardError(
            "SELECT list must be a column, number, or COUNT(*)".to_string(),
        )),
    }
}

fn check_where(expr: &Expr, dialect: SqlDialect) -> Result<(), GuardError> {
    match expr {
        Expr::Identifier(ident) => check_ident(ident, dialect),
        Expr::CompoundIdentifier(_) => Err(GuardError(
            "qualified column names are not allowed".to_string(),
        )),
        Expr::Nested(inner) => check_where(inner, dialect),
        Expr::Value(value) if is_number(value) || is_placeholder(value) => Ok(()),
        Expr::BinaryOp { left, op, right } if is_allowed_operator(op) => {
            check_where(left, dialect)?;
            check_where(right, dialect)
        }
        Expr::Like {
            negated: false,
            any: false,
            expr,
            pattern,
            escape_char: None,
        } => {
            check_where(expr, dialect)?;
            check_where(pattern, dialect)
        }
        Expr::Between {
            expr,
            negated: false,
            low,
            high,
        } => {
            check_where(expr, dialect)?;
            check_where(low, dialect)?;
            check_where(high, dialect)
        }
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            check_where(expr, dialect)?;
            for item in list {
                check_where(item, dialect)?;
            }
            Ok(())
        }
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => check_where(inner, dialect),
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
        && !ident.value.contains('@')
}

fn check_object_name(name: &ObjectName, dialect: SqlDialect) -> Result<(), GuardError> {
    if name.0.is_empty() {
        return Err(GuardError("missing table name".to_string()));
    }
    // `SELECT 1 FROM DUAL` is the only unquoted Oracle table name.
    if dialect == SqlDialect::Oracle
        && name.0.len() == 1
        && name
            .0
            .first()
            .and_then(|part| part.as_ident())
            .is_some_and(|ident| ident.value.eq_ignore_ascii_case("dual"))
    {
        return Ok(());
    }
    for part in &name.0 {
        let Some(ident) = part.as_ident() else {
            return Err(GuardError("table name must be an identifier".to_string()));
        };
        check_ident(ident, dialect)?;
    }
    Ok(())
}

fn check_ident(ident: &Ident, dialect: SqlDialect) -> Result<(), GuardError> {
    if ident.value.contains('@') {
        return Err(GuardError("database links are not allowed".to_string()));
    }
    if ident.value.eq_ignore_ascii_case("nextval") || ident.value.eq_ignore_ascii_case("currval") {
        return Err(GuardError(
            "sequence references are not allowed".to_string(),
        ));
    }
    if ident.quote_style.is_none() && is_bare_pseudocolumn(ident) {
        return Err(GuardError("function calls are not allowed".to_string()));
    }
    if dialect == SqlDialect::Oracle && ident.quote_style != Some('"') {
        return Err(GuardError(
            "Oracle identifiers must be double-quoted".to_string(),
        ));
    }
    Ok(())
}

/// The only overlap aggregate Dipper will run. `GuardedSelect::parse` still rejects it.
#[derive(Debug, Clone)]
pub struct GuardedOverlapSelect {
    sql: String,
    dialect: SqlDialect,
}

impl GuardedOverlapSelect {
    pub fn parse(sql: &str, dialect: SqlDialect) -> Result<Self, GuardError> {
        reject_comments(sql, dialect)?;
        let parsed = Parser::parse_sql(dialect_impl(dialect).as_ref(), sql)
            .map_err(|err| GuardError(err.to_string()))?;
        if parsed.len() != 1 {
            return Err(GuardError("exactly one statement is required".to_string()));
        }
        let Statement::Query(query) = &parsed[0] else {
            return Err(GuardError("overlap check must be a SELECT".to_string()));
        };
        check_overlap_query(query, dialect)?;
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

fn check_overlap_query(query: &Query, dialect: SqlDialect) -> Result<(), GuardError> {
    reject_query_extras(query)?;
    let select = plain_select(query)?;
    reject_select_extras(select)?;
    if select.projection.len() != 1 || !overlap_projects_count(&select.projection[0]) {
        return Err(GuardError(
            "overlap check must project COUNT(*)".to_string(),
        ));
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(GuardError("JOIN is not allowed".to_string()));
    }
    require_alias(&select.from[0].relation, dialect, "A")?;
    let Some(selection) = &select.selection else {
        return Err(GuardError("overlap check needs a WHERE".to_string()));
    };
    let mut nulls = Vec::new();
    let mut exists = None;
    collect_overlap_terms(selection, &mut nulls, &mut exists)?;
    if nulls.is_empty() || exists.is_none() {
        return Err(GuardError(
            "overlap check must test NOT NULL and EXISTS".to_string(),
        ));
    }
    let exists = exists.unwrap();
    check_exists(exists, dialect, &nulls)?;
    Ok(())
}

fn overlap_projects_count(item: &SelectItem) -> bool {
    let SelectItem::UnnamedExpr(Expr::Function(function)) = item else {
        return false;
    };
    is_count_star(function)
}

fn collect_overlap_terms<'a>(
    expr: &'a Expr,
    nulls: &mut Vec<&'a Expr>,
    exists: &mut Option<&'a Query>,
) -> Result<(), GuardError> {
    match expr {
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::And => {
            collect_overlap_terms(left, nulls, exists)?;
            collect_overlap_terms(right, nulls, exists)
        }
        Expr::IsNotNull(inner) => {
            nulls.push(inner);
            Ok(())
        }
        Expr::Exists { subquery, negated } if !negated => {
            if exists.replace(subquery.as_ref()).is_some() {
                return Err(GuardError("one EXISTS only".to_string()));
            }
            Ok(())
        }
        _ => Err(GuardError(
            "overlap WHERE must be NOT NULL and EXISTS".to_string(),
        )),
    }
}

fn check_exists(query: &Query, dialect: SqlDialect, nulls: &[&Expr]) -> Result<(), GuardError> {
    reject_query_extras(query)?;
    let select = plain_select(query)?;
    reject_select_extras(select)?;
    if select.projection.len() != 1 || !is_numeric_one(&select.projection[0]) {
        return Err(GuardError("EXISTS must project 1".to_string()));
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(GuardError("EXISTS cannot join".to_string()));
    }
    require_alias(&select.from[0].relation, dialect, "B")?;
    let Some(selection) = &select.selection else {
        return Err(GuardError("EXISTS needs a WHERE".to_string()));
    };
    let mut pairs = Vec::new();
    collect_equals(selection, &mut pairs)?;
    if pairs.len() != nulls.len() {
        return Err(GuardError(
            "EXISTS equalities must match the NOT NULL columns".to_string(),
        ));
    }
    for (null_expr, (left, right)) in nulls.iter().zip(pairs) {
        let source = qualified_column(null_expr, "A", dialect)?;
        let (target, matched) = match (
            qualified_column(left, "B", dialect),
            qualified_column(right, "A", dialect),
        ) {
            (Ok(target), Ok(matched)) => (target, matched),
            _ => {
                let target = qualified_column(right, "B", dialect)?;
                let matched = qualified_column(left, "A", dialect)?;
                (target, matched)
            }
        };
        if matched != source || target.is_empty() {
            return Err(GuardError(
                "EXISTS must compare the same quoted columns".to_string(),
            ));
        }
        let _ = target;
    }
    Ok(())
}

fn reject_query_extras(query: &Query) -> Result<(), GuardError> {
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || query.for_clause.is_some()
        || !query.locks.is_empty()
    {
        return Err(GuardError(
            "overlap check cannot have extra clauses".to_string(),
        ));
    }
    Ok(())
}

fn plain_select(query: &Query) -> Result<&Select, GuardError> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(GuardError("overlap check must be one SELECT".to_string()));
    };
    Ok(select)
}

fn reject_select_extras(select: &Select) -> Result<(), GuardError> {
    if select.flavor != SelectFlavor::Standard
        || select.distinct.is_some()
        || !select.optimizer_hints.is_empty()
        || select.select_modifiers.is_some()
        || select.top.is_some()
        || select.exclude.is_some()
        || select.into.is_some()
        || select.prewhere.is_some()
        || select.having.is_some()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || !select.lateral_views.is_empty()
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
    Ok(())
}

fn require_alias(
    relation: &TableFactor,
    dialect: SqlDialect,
    expected: &str,
) -> Result<(), GuardError> {
    let TableFactor::Table {
        name,
        alias,
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
        return Err(GuardError("overlap table must be a plain name".to_string()));
    };
    if !with_hints.is_empty() || !partitions.is_empty() || !index_hints.is_empty() {
        return Err(GuardError("table hints are not allowed".to_string()));
    }
    check_object_name(name, dialect)?;
    let Some(alias) = alias else {
        return Err(GuardError(format!("overlap table needs alias {expected}")));
    };
    if alias.name.value != expected || !alias.columns.is_empty() {
        return Err(GuardError(format!("overlap alias must be {expected}")));
    }
    check_ident(&alias.name, dialect)?;
    Ok(())
}

fn qualified_column<'a>(
    expr: &'a Expr,
    alias: &str,
    dialect: SqlDialect,
) -> Result<&'a str, GuardError> {
    let Expr::CompoundIdentifier(parts) = expr else {
        return Err(GuardError(
            "overlap columns must be quoted and qualified".to_string(),
        ));
    };
    if parts.len() != 2 || parts[0].value != alias {
        return Err(GuardError(
            "overlap columns must use the table alias".to_string(),
        ));
    }
    check_ident(&parts[0], dialect)?;
    check_ident(&parts[1], dialect)?;
    Ok(parts[1].value.as_str())
}

fn is_numeric_one(item: &SelectItem) -> bool {
    let SelectItem::UnnamedExpr(expr) = item else {
        return false;
    };
    match expr {
        Expr::Value(ValueWithSpan {
            value: Value::Number(number, _),
            ..
        }) => number == "1",
        _ => false,
    }
}

fn collect_equals<'a>(
    expr: &'a Expr,
    pairs: &mut Vec<(&'a Expr, &'a Expr)>,
) -> Result<(), GuardError> {
    match expr {
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::And => {
            collect_equals(left, pairs)?;
            collect_equals(right, pairs)
        }
        Expr::BinaryOp { left, op, right } if *op == BinaryOperator::Eq => {
            pairs.push((left, right));
            Ok(())
        }
        _ => Err(GuardError(
            "EXISTS WHERE must compare quoted columns".to_string(),
        )),
    }
}

fn is_bare_pseudocolumn(ident: &Ident) -> bool {
    matches!(
        ident.value.to_ascii_uppercase().as_str(),
        "SYSDATE"
            | "SYSTIMESTAMP"
            | "SESSIONTIMEZONE"
            | "ORA_ROWSCN"
            | "CURRENT_DATE"
            | "CURRENT_TIMESTAMP"
            | "CURRENT_TIME"
            | "LOCALTIMESTAMP"
            | "LOCALTIME"
            | "USER"
            | "UID"
            | "ROWNUM"
            | "LEVEL"
            | "F_AUDIT"
    )
}
