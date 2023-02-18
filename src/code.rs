use indoc::indoc;
use inflector::Inflector;

use crate::{GenerationConfig, TableOptions};
use crate::parser::{FILE_SIGNATURE, ParsedTableMacro};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StructType {
    Read,
    // this struct type maps directly to a database row
    Form, // this one contains primary key columns (not optional) and normal columns (optional) excluding those marked as autogenerated

    Update,
    Create,
}

impl StructType {
    pub fn prefix(&self) -> &'static str {
        match self {
            StructType::Read => "",
            StructType::Form => "",
            StructType::Update => "Update",
            StructType::Create => "Create",
        }
    }

    pub fn suffix(&self) -> &'static str {
        match self {
            StructType::Read => "",
            StructType::Form => "Form",
            StructType::Update => "",
            StructType::Create => "",
        }
    }

    /// returns a struct name for this struct type given a base name
    pub fn format(&self, name: &'_ str) -> String {
        format!(
            "{struct_prefix}{struct_name}{struct_suffix}",
            struct_prefix = self.prefix(),
            struct_name = name,
            struct_suffix = self.suffix()
        )
    }
}

struct Struct<'a> {
    identifier: String,
    ty: StructType,
    table: &'a ParsedTableMacro,
    opts: TableOptions<'a>,
    config: &'a GenerationConfig<'a>,
    rendered_code: Option<String>,
    has_fields: Option<bool>, // note: this is only correctly set after a call to render() which gets called in Struct::new()
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub base_type: String,

    pub is_optional: bool,
}

impl<'a> Struct<'a> {
    pub fn new(ty: StructType, table: &'a ParsedTableMacro, config: &'a GenerationConfig<'_>) -> Self {
        let mut obj = Self {
            identifier: ty.format(table.struct_name.as_str()),
            opts: config.table(&table.name.to_string()),
            table,
            ty,
            config,
            rendered_code: None,
            has_fields: None,
        };
        obj.render();
        obj
    }

    pub fn code(&self) -> &str {
        self.rendered_code.as_deref().unwrap_or_default()
    }

    pub fn has_fields(&self) -> bool {
        self.has_fields.unwrap()
    }

    fn attr_tsync(&self) -> &'static str {
        #[cfg(feature = "tsync")] match self.opts.get_tsync() {
            true => "#[tsync::tsync]\n",
            false => ""
        }
        #[cfg(not(feature = "tsync"))] ""
    }

    fn attr_derive(&self) -> String {
        format!("#[derive(Debug, Serialize, Deserialize, Clone, Queryable, Insertable{derive_aschangeset}{derive_identifiable}{derive_associations})]",
                derive_associations = match self.ty {
                    StructType::Read => {
                        if self.table.foreign_keys.len() > 0 { ", Associations" } else { "" }
                    }
                    _ => { "" }
                },
                derive_identifiable = match self.ty {
                    StructType::Read => {
                        if self.table.foreign_keys.len() > 0 { ", Identifiable" } else { "" }
                    }
                    _ => { "" }
                },
                derive_aschangeset = match self.ty {
                    _ => if self.fields().iter().all(|f| self.table.primary_key_column_names().contains(&f.name)) {""} else { ", AsChangeset" }
                }
        )
    }

    fn fields(&self) -> Vec<StructField> {
        self.table.columns.iter()
            .filter(|c| {
                let is_autogenerated = self.opts.autogenerated_columns.as_deref().unwrap_or_default().contains(&c.name.to_string().as_str());

                match self.ty {
                    StructType::Read => {
                        true
                    }
                    StructType::Form => {
                        true
                    }
                    StructType::Update => {
                        let is_pk = self.table.primary_key_columns.contains(&c.name);

                        !is_pk
                    }
                    StructType::Create => {
                        !is_autogenerated
                    }
                }
            })
            .map(|c| {
                let name = c.name.to_string();
                let base_type = if c.is_nullable { format!("Option<{}>", c.ty) } else { c.ty.clone() };
                let mut is_optional = false;

                let is_pk = self.table.primary_key_columns.iter().any(|pk| pk.to_string().eq(name.as_str()));
                let is_autogenerated = self.opts.autogenerated_columns.as_deref().unwrap_or_default().contains(&c.name.to_string().as_str());
                // let is_fk = table.foreign_keys.iter().any(|fk| fk.1.to_string().eq(field_name.as_str()));

                match self.ty {
                    StructType::Read => {}
                    StructType::Form => {}
                    StructType::Update => {
                        // all non-key fields should be optional in Form structs (to allow partial updates)
                        is_optional = !is_pk || (is_pk && is_autogenerated);
                    }
                    StructType::Create => {}
                }

                StructField {
                    name,
                    base_type,
                    is_optional,
                }
            })
            .collect()
    }

    fn render(&mut self) {
        let ty = self.ty;
        let table = &self.table;
        let opts = self.config.table(table.name.to_string().as_str());

        let primary_keys: Vec<String> = table.primary_key_column_names();

        let belongs_to = table
            .foreign_keys
            .iter()
            .map(|fk| {
                format!(
                    ", belongs_to({foreign_table_name}, foreign_key={join_column})",
                    foreign_table_name = fk.0.to_string().to_pascal_case().to_singular(),
                    join_column = fk.1
                )
            })
            .collect::<Vec<String>>()
            .join(" ");

        let struct_code = format!(
            indoc! {r#"
            {tsync_attr}{derive_attr}
            #[diesel(table_name={table_name}{primary_key}{belongs_to})]
            pub struct {struct_name} {{
            $COLUMNS$
            }}
        "#},
            tsync_attr = self.attr_tsync(),
            derive_attr = self.attr_derive(),
            table_name = table.name,
            struct_name = ty.format(table.struct_name.as_str()),
            primary_key = if ty != StructType::Read {
                "".to_string()
            } else {
                format!(", primary_key({})", primary_keys.join(","))
            },
            belongs_to = if ty != StructType::Read {
                "".to_string()
            } else {
                belongs_to
            }
        );

        let fields = self.fields();
        let mut lines = vec![];
        for f in fields.iter() {
            let field_name = &f.name;
            let field_type = if f.is_optional { format!("Option<{}>", f.base_type) } else { f.base_type.clone() };

            lines.push(format!(r#"    pub {field_name}: {field_type},"#));
        }

        if fields.is_empty() {
            self.has_fields = Some(false);
            self.rendered_code = Some("".to_string());
        } else {
            self.has_fields = Some(true);
            self.rendered_code = Some(struct_code.replace("$COLUMNS$", &lines.join("\n")));
        }
    }
}

fn build_table_fns(table: &ParsedTableMacro, config: &GenerationConfig, create_struct: Struct, update_struct: Struct) -> String {
    let table_options = config.table(&table.name.to_string());

    let primary_column_name_and_type: Vec<(String, String)> = table
        .primary_key_columns
        .iter()
        .map(|pk| {
            let col = table
                .columns
                .iter()
                .find(|it| it.name.to_string().eq(pk.to_string().as_str()))
                .expect("Primary key column doesn't exist in table");

            (col.name.to_string(), col.ty.to_string())
        })
        .collect();

    let item_id_params = primary_column_name_and_type
        .iter()
        .map(|name_and_type| {
            format!(
                "param_{name}: {ty}",
                name = name_and_type.0,
                ty = name_and_type.1
            )
        })
        .collect::<Vec<String>>()
        .join(", ");
    let item_id_filters = primary_column_name_and_type
        .iter()
        .map(|name_and_type| {
            format!(
                "filter({name}.eq(param_{name}))",
                name = name_and_type.0.to_string()
            )
        })
        .collect::<Vec<String>>()
        .join(".");

    // template variables
    let table_name = table.name.to_string();
    #[cfg(feature = "tsync")] let tsync = match table_options.get_tsync() {
        true => "#[tsync::tsync]",
        false => ""
    };
    #[cfg(not(feature = "tsync"))] let tsync = "";
    let struct_name = &table.struct_name;
    let create_struct_identifier = &create_struct.identifier;
    let update_struct_identifier = &update_struct.identifier;
    let item_id_params = item_id_params;
    let item_id_filters = item_id_filters;

    let mut buffer = String::new();

    buffer.push_str(&format!(r##"{tsync}
#[derive(Serialize)]
pub struct PaginationResult<T> {{
    pub items: Vec<T>,
    pub total_items: i64,
    /// 0-based index
    pub page: i64,
    pub page_size: i64,
    pub num_pages: i64,
}}
"##));

    buffer.push_str(&format!(r##"
impl {struct_name} {{
"##));

    if create_struct.has_fields() {
        buffer.push_str(&format!(r##"
    pub fn create(db: &mut Connection, item: &{create_struct_identifier}) -> QueryResult<Self> {{
        use crate::schema::{table_name}::dsl::*;

        insert_into({table_name}).values(item).get_result::<Self>(db)
    }}
"##));
    } else {
        buffer.push_str(&format!(r##"
    pub fn create(db: &mut Connection) -> QueryResult<Self> {{
        use crate::schema::{table_name}::dsl::*;

        insert_into({table_name}).default_values().get_result::<Self>(db)
    }}
"##));
    }

    buffer.push_str(&format!(r##"
    pub fn read(db: &mut Connection, {item_id_params}) -> QueryResult<Self> {{
        use crate::schema::{table_name}::dsl::*;

        {table_name}.{item_id_filters}.first::<Self>(db)
    }}
"##));


    buffer.push_str(&format!(r##"
    /// Paginates through the table where page is a 0-based index (i.e. page 0 is the first page)
    pub fn paginate(db: &mut Connection, page: i64, page_size: i64) -> QueryResult<PaginationResult<Self>> {{
        use crate::schema::{table_name}::dsl::*;

        let page_size = if page_size < 1 {{ 1 }} else {{ page_size }};
        let total_items = {table_name}.count().get_result(db)?;
        let items = {table_name}.limit(page_size).offset(page * page_size).load::<Self>(db)?;

        Ok(PaginationResult {{
            items,
            total_items,
            page,
            page_size,
            /* ceiling division of integers */
            num_pages: total_items / page_size + i64::from(total_items % page_size != 0)
        }})
    }}
"##));

    // TODO: If primary key columns are attached to the form struct (not optionally)
    // then don't require item_id_params (otherwise it'll be duplicated)

    // if has_update_struct {
    if update_struct.has_fields() {
        // It's possible we have a form struct with all primary keys (for example, for a join table).
        // In this scenario, we also have to check whether there are any updatable columns for which
        // we should generate an update() method.

        buffer.push_str(&format!(r##"
    pub fn update(db: &mut Connection, {item_id_params}, item: &{update_struct_identifier}) -> QueryResult<Self> {{
        use crate::schema::{table_name}::dsl::*;

        diesel::update({table_name}.{item_id_filters}).set(item).get_result(db)
    }}
"##));
    }

    buffer.push_str(&format!(r##"
    pub fn delete(db: &mut Connection, {item_id_params}) -> QueryResult<usize> {{
        use crate::schema::{table_name}::dsl::*;

        diesel::delete({table_name}.{item_id_filters}).execute(db)
    }}
"##));

    buffer.push_str(&format!(r##"
}}"##));

    buffer
}

fn build_imports(table: &ParsedTableMacro, config: &GenerationConfig) -> String {
    let belongs_imports = table
        .foreign_keys
        .iter()
        .map(|fk| {
            format!(
                "use crate::models::{foreign_table_name_model}::{singular_struct_name};",
                foreign_table_name_model = fk.0.to_string().to_snake_case().to_lowercase(),
                singular_struct_name = fk.0.to_string().to_pascal_case().to_singular()
            )
        })
        .collect::<Vec<String>>()
        .join("\n");

    format!(
        indoc! {"
        use crate::diesel::*;
        use crate::schema::*;
        use diesel::QueryResult;
        use serde::{{Deserialize, Serialize}};
        {belongs_imports}

        type Connection = {connection_type};
    "},
        connection_type = config.connection_type,
        belongs_imports = belongs_imports,
    )
}

pub fn generate_for_table(table: ParsedTableMacro, config: &GenerationConfig) -> String {
    // first, we generate struct code
    let read_struct = Struct::new(StructType::Read, &table, config);
    let update_struct = Struct::new(StructType::Update, &table, config);
    let create_struct = Struct::new(StructType::Create, &table, config);

    let mut structs = String::new();
    structs.push_str(&read_struct.code());
    structs.push('\n');
    structs.push_str(&create_struct.code());
    structs.push('\n');
    structs.push_str(&update_struct.code());

    let functions = build_table_fns(&table, config, create_struct, update_struct);
    let imports = build_imports(&table, config);

    format!("{FILE_SIGNATURE}\n\n{imports}\n{structs}\n{functions}")
}
