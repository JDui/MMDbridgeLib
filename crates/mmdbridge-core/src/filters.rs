use chrono::Utc;
use rusqlite::{OptionalExtension, params, types::Value as SqlValue};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    CoreError, CoreResult, Library, scanner,
    types::{AssetCursor, AssetPage, FilterExpr, FilterField, FilterOperator, SavedFilter},
};

pub(crate) fn list(library: &Library) -> CoreResult<Vec<SavedFilter>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT id,name,expression_json,created_at,updated_at FROM saved_filters ORDER BY name COLLATE NOCASE",
    )?;
    let rows = statement.query_map([], |row| {
        let expression_json: String = row.get(2)?;
        let expression = serde_json::from_str(&expression_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                expression_json.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(SavedFilter {
            id: row.get(0)?,
            name: row.get(1)?,
            expression,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn save(
    library: &Library,
    filter_id: Option<&str>,
    name: &str,
    expression: FilterExpr,
) -> CoreResult<SavedFilter> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(CoreError::InvalidFilter(
            "filter name must contain 1 to 100 characters and no control characters".to_owned(),
        ));
    }
    compile(&expression)?;
    let expression_json = serde_json::to_string(&expression)?;
    let now = Utc::now().to_rfc3339();
    let id = filter_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let connection = library.connection()?;
    let created_at = if let Some(filter_id) = filter_id {
        let created_at: Option<String> = connection
            .query_row(
                "SELECT created_at FROM saved_filters WHERE id=?1",
                [filter_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(created_at) = created_at else {
            return Err(CoreError::InvalidFilter(format!(
                "saved filter was not found: {filter_id}"
            )));
        };
        let changed = connection.execute(
            "UPDATE saved_filters SET name=?2,expression_json=?3,updated_at=?4 WHERE id=?1",
            params![filter_id, name, expression_json, now],
        )?;
        if changed == 0 {
            return Err(CoreError::InvalidFilter(format!(
                "saved filter was not found: {filter_id}"
            )));
        }
        created_at
    } else {
        connection.execute(
            "INSERT INTO saved_filters(id,name,expression_json,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?4)",
            params![id, name, expression_json, now],
        )?;
        now.clone()
    };
    drop(connection);
    Ok(SavedFilter {
        id,
        name: name.to_owned(),
        expression,
        created_at,
        updated_at: now,
    })
}

pub(crate) fn remove(library: &Library, filter_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    Ok(connection.execute("DELETE FROM saved_filters WHERE id=?1", [filter_id])? > 0)
}

pub(crate) fn apply(
    library: &Library,
    filter_id: &str,
    query: Option<&str>,
    limit: usize,
) -> CoreResult<Vec<crate::types::Asset>> {
    let expression_json: Option<String> = library
        .connection()?
        .query_row(
            "SELECT expression_json FROM saved_filters WHERE id=?1",
            [filter_id],
            |row| row.get(0),
        )
        .optional()?;
    let expression_json = expression_json.ok_or_else(|| {
        CoreError::InvalidFilter(format!("saved filter was not found: {filter_id}"))
    })?;
    let expression: FilterExpr = serde_json::from_str(&expression_json)?;
    let (predicate, values) = compile(&expression)?;
    scanner::list_assets_with_predicate(library, query, limit, &predicate, values)
}

pub(crate) fn apply_page(
    library: &Library,
    filter_id: &str,
    query: Option<&str>,
    root_id: Option<&str>,
    cursor: Option<&AssetCursor>,
    limit: usize,
) -> CoreResult<AssetPage> {
    let expression_json: Option<String> = library
        .connection()?
        .query_row(
            "SELECT expression_json FROM saved_filters WHERE id=?1",
            [filter_id],
            |row| row.get(0),
        )
        .optional()?;
    let expression_json = expression_json.ok_or_else(|| {
        CoreError::InvalidFilter(format!("saved filter was not found: {filter_id}"))
    })?;
    let expression: FilterExpr = serde_json::from_str(&expression_json)?;
    let (predicate, values) = compile(&expression)?;
    scanner::list_asset_page_with_predicate(
        library, query, root_id, limit, &predicate, values, cursor,
    )
}

fn compile(expression: &FilterExpr) -> CoreResult<(String, Vec<SqlValue>)> {
    compile_after(expression, 2)
}

pub(crate) fn compile_after(expression: &FilterExpr, first_parameter: usize) -> CoreResult<(String, Vec<SqlValue>)> {
    let mut values = Vec::new();
    let mut next_parameter = first_parameter;
    let mut node_count = 0usize;
    let sql = compile_node(
        expression,
        &mut values,
        &mut next_parameter,
        &mut node_count,
        0,
    )?;
    Ok((sql, values))
}

fn compile_node(
    expression: &FilterExpr,
    values: &mut Vec<SqlValue>,
    next_parameter: &mut usize,
    node_count: &mut usize,
    depth: usize,
) -> CoreResult<String> {
    *node_count += 1;
    if depth > 8 || *node_count > 64 {
        return Err(CoreError::InvalidFilter(
            "filter expressions may have at most 64 nodes and 8 nested groups".to_owned(),
        ));
    }
    match expression {
        FilterExpr::And { children } | FilterExpr::Or { children } => {
            if children.is_empty() || children.len() > 32 {
                return Err(CoreError::InvalidFilter(
                    "AND/OR groups must contain 1 to 32 child expressions".to_owned(),
                ));
            }
            let joiner = if matches!(expression, FilterExpr::And { .. }) {
                " AND "
            } else {
                " OR "
            };
            let compiled = children
                .iter()
                .map(|child| compile_node(child, values, next_parameter, node_count, depth + 1))
                .collect::<CoreResult<Vec<_>>>()?;
            Ok(format!("({})", compiled.join(joiner)))
        }
        FilterExpr::Not { child } => Ok(format!(
            "(NOT {})",
            compile_node(child, values, next_parameter, node_count, depth + 1)?
        )),
        FilterExpr::Rule {
            field,
            operator,
            value,
        } => compile_rule(field, *operator, value, values, next_parameter),
    }
}

fn compile_rule(
    field: &FilterField,
    operator: FilterOperator,
    value: &Value,
    values: &mut Vec<SqlValue>,
    next_parameter: &mut usize,
) -> CoreResult<String> {
    if matches!(field, FilterField::NeedsReview) {
        return Err(CoreError::InvalidFilter(
            "needsReview is retired; recreate this saved filter without the removed condition"
                .to_owned(),
        ));
    }
    if matches!(field, FilterField::DuplicateStatus) {
        return Err(CoreError::InvalidFilter(
            "duplicateStatus is retired; recreate this saved filter without the removed condition"
                .to_owned(),
        ));
    }
    if matches!(field, FilterField::CameraOnly) {
        return Err(CoreError::InvalidFilter(
            "cameraOnly is retired; pure Camera files are hidden from asset collections"
                .to_owned(),
        ));
    }
    if matches!(field, FilterField::FileType)
        && value.as_str().is_some_and(|value| {
            value.trim().trim_start_matches('.').eq_ignore_ascii_case("x")
        })
    {
        return Err(CoreError::InvalidFilter(
            "fileType X is retired; recreate this saved filter without the removed condition"
                .to_owned(),
        ));
    }
    if matches!(field, FilterField::Tag) {
        return compile_tag_rule(operator, value, values, next_parameter);
    }
    let (expression, value_kind) = field_sql(field);
    let sql_value = to_sql_value(value, value_kind)?;
    if matches!(field, FilterField::AssetType)
        && !matches!(value.as_str(), Some("model" | "motion" | "scene"))
    {
        return Err(CoreError::InvalidFilter(
            "assetType must be model, motion, or scene".to_owned(),
        ));
    }
    if matches!(field, FilterField::CardStatus)
        && !matches!(
            value.as_str(),
            Some("CardValid" | "CardMissing" | "CardStale" | "CardBroken")
        )
    {
        return Err(CoreError::InvalidFilter(
            "cardStatus must be CardValid, CardMissing, CardStale, or CardBroken".to_owned(),
        ));
    }
    if matches!(operator, FilterOperator::Contains) {
        if value_kind != ValueKind::Text {
            return Err(CoreError::InvalidFilter(
                "contains is only valid for text fields".to_owned(),
            ));
        }
    } else if matches!(
        operator,
        FilterOperator::Gt | FilterOperator::Gte | FilterOperator::Lt | FilterOperator::Lte
    ) && !matches!(value_kind, ValueKind::Number | ValueKind::Date)
    {
        return Err(CoreError::InvalidFilter(
            "ordered comparisons require a numeric or date field".to_owned(),
        ));
    }
    let parameter = format!("?{}", *next_parameter);
    *next_parameter += 1;
    let clause = match operator {
        FilterOperator::Eq => format!("({expression} = {parameter})"),
        FilterOperator::Ne => format!("({expression} <> {parameter})"),
        FilterOperator::Contains => {
            format!("({expression} LIKE {parameter} ESCAPE '\\' COLLATE NOCASE)")
        }
        FilterOperator::Gt => format!("({expression} > {parameter})"),
        FilterOperator::Gte => format!("({expression} >= {parameter})"),
        FilterOperator::Lt => format!("({expression} < {parameter})"),
        FilterOperator::Lte => format!("({expression} <= {parameter})"),
    };
    values.push(if matches!(operator, FilterOperator::Contains) {
        let SqlValue::Text(text) = sql_value else {
            unreachable!("text field validation ensures string values")
        };
        SqlValue::Text(format!("%{}%", escape_like(&text)))
    } else {
        sql_value
    });
    Ok(clause)
}

fn compile_tag_rule(
    operator: FilterOperator,
    value: &Value,
    values: &mut Vec<SqlValue>,
    next_parameter: &mut usize,
) -> CoreResult<String> {
    if !matches!(
        operator,
        FilterOperator::Eq | FilterOperator::Ne | FilterOperator::Contains
    ) {
        return Err(CoreError::InvalidFilter(
            "tag filters support eq, ne, or contains".to_owned(),
        ));
    }
    let SqlValue::Text(value) = to_sql_value(value, ValueKind::Text)? else {
        unreachable!("text filter validation ensures string values")
    };
    let parameter = format!("?{}", *next_parameter);
    *next_parameter += 1;
    let (comparison, bound_value) = match operator {
        FilterOperator::Eq | FilterOperator::Ne => {
            (format!("t.name = {parameter}"), SqlValue::Text(value))
        }
        FilterOperator::Contains => (
            format!("t.name LIKE {parameter} ESCAPE '\\' COLLATE NOCASE"),
            SqlValue::Text(format!("%{}%", escape_like(&value))),
        ),
        _ => unreachable!("operator was validated above"),
    };
    values.push(bound_value);
    let predicate = format!(
        "EXISTS(SELECT 1 FROM asset_tags at JOIN tags t ON t.id=at.tag_id WHERE at.asset_id=a.id AND {comparison})"
    );
    Ok(if matches!(operator, FilterOperator::Ne) {
        format!("(NOT {predicate})")
    } else {
        format!("({predicate})")
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Text,
    Number,
    Boolean,
    Date,
}

fn field_sql(field: &FilterField) -> (String, ValueKind) {
    match field {
        FilterField::AssetType => ("a.asset_type".to_owned(), ValueKind::Text),
        FilterField::RootId => ("a.root_id".to_owned(), ValueKind::Text),
        FilterField::Directory => ("a.asset_directory".to_owned(), ValueKind::Text),
        FilterField::Tag => unreachable!("tag rules are compiled as correlated subqueries"),
        FilterField::Favorite => (
            "EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id)".to_owned(),
            ValueKind::Boolean,
        ),
        FilterField::CardStatus => ("COALESCE(c.status,'CardMissing')".to_owned(), ValueKind::Text),
        FilterField::DuplicateStatus | FilterField::CameraOnly | FilterField::NeedsReview => {
            unreachable!("retired filter fields are rejected above")
        }
        FilterField::RelationStatus => (
            "EXISTS(SELECT 1 FROM relations rel WHERE rel.source_asset=a.id OR rel.target_asset=a.id)".to_owned(),
            ValueKind::Boolean,
        ),
        FilterField::RecentlyAdded => ("a.created_at".to_owned(), ValueKind::Date),
        FilterField::RecentlyModified => ("a.updated_at".to_owned(), ValueKind::Date),
        FilterField::PolygonCount => ("json_extract(m.value_json,'$.polygon_count')".to_owned(), ValueKind::Number),
        FilterField::BoneCount => ("json_extract(m.value_json,'$.bone_count')".to_owned(), ValueKind::Number),
        FilterField::SkeletonClass => ("COALESCE(json_extract(m.value_json,'$.skeleton_class'),'unknown')".to_owned(), ValueKind::Text),
        FilterField::HasThumbnail => (
            "COALESCE(c.status,'CardMissing') IN ('CardValid','CardStale') AND json_extract(c.manifest_json,'$.thumbnail.file') IS NOT NULL".to_owned(),
            ValueKind::Boolean,
        ),
        FilterField::HasCard => (
            "COALESCE(c.status,'CardMissing') IN ('CardValid','CardStale')".to_owned(),
            ValueKind::Boolean,
        ),
        FilterField::FrameCount => ("json_extract(m.value_json,'$.total_frames')".to_owned(), ValueKind::Number),
        FilterField::Duration => ("json_extract(m.value_json,'$.duration_seconds')".to_owned(), ValueKind::Number),
        FilterField::HasBoneMotion => (metadata_bool("has_bone_motion"), ValueKind::Boolean),
        FilterField::HasMorphMotion => (metadata_bool("has_morph_motion"), ValueKind::Boolean),
        FilterField::HasCamera => (metadata_bool("has_camera"), ValueKind::Boolean),
        FilterField::Pose => (metadata_bool("is_pose"), ValueKind::Boolean),
        FilterField::HasPairedCamera => (
            "EXISTS(SELECT 1 FROM relations rel WHERE rel.relation_type='MotionCameraPair' AND (rel.source_asset=a.id OR rel.target_asset=a.id))".to_owned(),
            ValueKind::Boolean,
        ),
        FilterField::FileType => ("json_extract(m.value_json,'$.file_type')".to_owned(), ValueKind::Text),
        FilterField::Width => ("json_extract(m.value_json,'$.width')".to_owned(), ValueKind::Number),
        FilterField::Depth => ("json_extract(m.value_json,'$.depth')".to_owned(), ValueKind::Number),
        FilterField::Area => ("json_extract(m.value_json,'$.area')".to_owned(), ValueKind::Number),
    }
}

fn metadata_bool(key: &str) -> String {
    format!("COALESCE(json_extract(m.value_json,'$.{key}'),0) != 0")
}

fn to_sql_value(value: &Value, kind: ValueKind) -> CoreResult<SqlValue> {
    match (kind, value) {
        (ValueKind::Text | ValueKind::Date, Value::String(value)) => {
            Ok(SqlValue::Text(value.clone()))
        }
        (ValueKind::Number, Value::Number(value)) => {
            value.as_f64().map(SqlValue::Real).ok_or_else(|| {
                CoreError::InvalidFilter("numeric rule value is out of range".to_owned())
            })
        }
        (ValueKind::Boolean, Value::Bool(value)) => Ok(SqlValue::Integer(i64::from(*value))),
        _ => Err(CoreError::InvalidFilter(
            "filter value does not match its field type".to_owned(),
        )),
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
