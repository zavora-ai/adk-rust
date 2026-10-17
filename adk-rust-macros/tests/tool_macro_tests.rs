use adk_core::{AdkError, Tool};
use adk_rust_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

// === Test 1: Basic tool with typed args ===

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to look up
    city: String,
}

/// Get the current weather for a city.
#[tool]
async fn get_weather(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "temp": 72, "city": args.city }))
}

#[test]
fn tool_has_correct_name() {
    let tool = GetWeather;
    assert_eq!(tool.name(), "get_weather");
}

#[test]
fn tool_has_doc_description() {
    let tool = GetWeather;
    assert_eq!(tool.description(), "Get the current weather for a city.");
}

#[test]
fn tool_has_schema() {
    let tool = GetWeather;
    let schema = tool.parameters_schema();
    assert!(schema.is_some());
    let schema = schema.unwrap();
    assert!(schema.get("properties").is_some());
}

// === Test 2: Tool with context parameter ===

/// Calculate a mathematical expression.
#[tool]
async fn calculator(
    _ctx: Arc<dyn adk_core::ToolContext>,
    args: CalcArgs,
) -> Result<Value, AdkError> {
    Ok(json!({ "result": args.a + args.b }))
}

#[derive(Deserialize, JsonSchema)]
struct CalcArgs {
    a: f64,
    b: f64,
}

#[test]
fn tool_with_context_has_correct_name() {
    let tool = Calculator;
    assert_eq!(tool.name(), "calculator");
}

#[test]
fn tool_with_context_has_schema() {
    let tool = Calculator;
    let schema = tool.parameters_schema().unwrap();
    let props = schema.get("properties").unwrap();
    assert!(props.get("a").is_some());
    assert!(props.get("b").is_some());
}

// === Test 3: Tool without doc comment gets auto-description ===

#[tool]
async fn my_cool_tool(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_without_doc_gets_auto_description() {
    let tool = MyCoolTool;
    assert_eq!(tool.description(), "my cool tool");
}

// === Test 4: Tool with read_only attribute ===

/// A read-only lookup tool.
#[tool(read_only)]
async fn read_only_lookup(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_read_only_attribute() {
    let tool = ReadOnlyLookup;
    assert!(tool.is_read_only(), "read_only attribute should set is_read_only() to true");
    assert!(!tool.is_concurrency_safe(), "concurrency_safe should remain false");
    assert!(!tool.is_long_running(), "long_running should remain false");
}

// === Test 5: Tool with multiple attributes ===

/// A concurrent read-only tool.
#[tool(read_only, concurrency_safe)]
async fn concurrent_lookup(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_multiple_attributes() {
    let tool = ConcurrentLookup;
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    assert!(!tool.is_long_running());
}

// === Test 6: Tool with long_running attribute ===

/// A long-running background task.
#[tool(long_running)]
async fn background_task(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_long_running_attribute() {
    let tool = BackgroundTask;
    assert!(!tool.is_read_only());
    assert!(!tool.is_concurrency_safe());
    assert!(tool.is_long_running());
}

// === Test 7: Tool with all attributes ===

/// A tool with every attribute set.
#[tool(read_only, concurrency_safe, long_running)]
async fn fully_attributed(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_all_attributes() {
    let tool = FullyAttributed;
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    assert!(tool.is_long_running());
}

// === Test 8: Plain #[tool] still defaults to false ===

#[test]
fn tool_no_attributes_defaults_false() {
    let tool = GetWeather;
    assert!(!tool.is_read_only());
    assert!(!tool.is_concurrency_safe());
    assert!(!tool.is_long_running());
    assert_eq!(tool.effect(), adk_tool::ToolEffect::NonIdempotent);
}

// === Test 8b: idempotent maps to ToolEffect::Idempotent ===

/// Sets the thermostat; repeating it changes nothing.
#[tool(idempotent)]
async fn set_thermostat(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

/// A read-only lookup also marked idempotent.
#[tool(read_only, idempotent)]
async fn idempotent_lookup(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_effect_attributes() {
    assert_eq!(SetThermostat.effect(), adk_tool::ToolEffect::Idempotent);
    assert!(!SetThermostat.is_read_only());
    assert_eq!(ReadOnlyLookup.effect(), adk_tool::ToolEffect::ReadOnly);
    assert_eq!(IdempotentLookup.effect(), adk_tool::ToolEffect::ReadOnly);
}

// === Test 9: Arg type names containing "Arc" are still args ===

#[derive(Deserialize, JsonSchema)]
struct ArchReviewArgs {
    rps: u32,
}

/// Review architecture capacity.
#[tool]
async fn review_architecture(args: ArchReviewArgs) -> Result<Value, AdkError> {
    Ok(json!({ "rps": args.rps }))
}

#[test]
fn tool_arg_type_name_containing_arc_is_not_skipped() {
    let tool = ReviewArchitecture;
    let schema = tool.parameters_schema().unwrap();
    let props = schema.get("properties").unwrap();
    assert!(props.get("rps").is_some());
}

// === Test 10: Arguments are checked against the schema before the function runs ===

static BOOKINGS: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

/// A postal address.
#[derive(Deserialize, JsonSchema)]
struct Address {
    city: String,
}

#[derive(Deserialize, JsonSchema)]
struct BookArgs {
    /// Room code such as `A1`.
    #[schemars(length(min = 2, max = 4), pattern(r"^[A-Z][0-9]{1,3}$"))]
    room: String,
    /// Slots to book.
    #[schemars(range(min = 1, max = 10))]
    count: u32,
    /// Where to send the confirmation.
    confirm_to: Address,
    /// Free-form note.
    note: Option<String>,
}

/// Book slots.
#[tool]
async fn book(args: BookArgs) -> Result<Value, AdkError> {
    let booking = json!({
        "room": args.room,
        "count": args.count,
        "city": args.confirm_to.city,
        "noted": args.note.is_some(),
    });
    BOOKINGS.lock().unwrap().push(booking.clone());
    Ok(booking)
}

#[derive(Deserialize, JsonSchema)]
struct LegacyArgs {
    #[schemars(range(max = 10))]
    count: u32,
}

/// Book slots, accepting whatever serde accepts.
#[tool(lenient_args)]
async fn legacy_book(args: LegacyArgs) -> Result<Value, AdkError> {
    Ok(json!({ "count": args.count }))
}

fn ctx() -> Arc<dyn adk_core::ToolContext> {
    Arc::new(adk_tool::SimpleToolContext::new("test"))
}

async fn run_book(args: Value) -> (Result<Value, AdkError>, usize) {
    let before = BOOKINGS.lock().unwrap().len();
    let result = Book.execute(ctx(), args).await;
    (result, BOOKINGS.lock().unwrap().len() - before)
}

fn valid() -> Value {
    json!({ "room": "A1", "count": 2, "confirm_to": { "city": "Nairobi" } })
}

fn rejected(result: Result<Value, AdkError>, ran: usize, expected: &[&str]) {
    let err = result.expect_err("arguments should be rejected");
    assert_eq!(ran, 0, "the function must not run");
    assert_eq!(err.code, adk_tool::TOOL_INVALID_ARGUMENTS_CODE);
    assert_eq!(err.category, adk_core::ErrorCategory::InvalidInput);
    for fragment in expected {
        assert!(err.message.contains(fragment), "{fragment:?} missing from {:?}", err.message);
    }
}

#[tokio::test]
async fn valid_arguments_run_once() {
    let (result, ran) = run_book(valid()).await;
    assert_eq!(
        result.unwrap(),
        json!({ "room": "A1", "count": 2, "city": "Nairobi", "noted": false })
    );
    assert_eq!(ran, 1);
}

#[tokio::test]
async fn null_is_accepted_for_an_optional_field() {
    let mut args = valid();
    args["note"] = Value::Null;
    let (result, ran) = run_book(args).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(ran, 1);
}

#[tokio::test]
async fn unknown_keys_are_rejected_at_every_level() {
    let mut args = valid();
    args["dry_run"] = json!(true);
    let (result, ran) = run_book(args).await;
    rejected(result, ran, &["invalid arguments for 'book'", "dry_run"]);

    let mut nested = valid();
    nested["confirm_to"]["zip"] = json!("00100");
    let (result, ran) = run_book(nested).await;
    rejected(result, ran, &["/confirm_to", "zip"]);
}

#[tokio::test]
async fn schema_bounds_are_enforced() {
    let mut args = valid();
    args["count"] = json!(50);
    args["room"] = json!("lobby");
    let (result, ran) = run_book(args).await;
    rejected(result, ran, &["/count: 50", "maximum of 10", "/room"]);

    let (result, ran) = run_book(json!({ "room": "A1", "count": 0, "confirm_to": {} })).await;
    rejected(result, ran, &["/count: 0", "/confirm_to", "city"]);
}

#[test]
fn the_declared_schema_is_left_open_for_providers() {
    // Only the validation schema is closed; Gemini rejects `additionalProperties`.
    let schema = Book.parameters_schema().unwrap();
    assert!(schema.get("additionalProperties").is_none());
    assert!(schema["$defs"]["Address"].get("additionalProperties").is_none());
    assert_eq!(
        schema["properties"]["confirm_to"]["description"],
        "Where to send the confirmation."
    );
    assert_eq!(schema["properties"]["count"]["maximum"], 10);
    assert!(schema.get("$schema").is_none());
    // The model-facing schema drops the `null` variant of an optional field.
    assert_eq!(schema["properties"]["note"]["type"], "string");
}

#[tokio::test]
async fn lenient_args_keeps_serde_only_behaviour() {
    let result = LegacyBook.execute(ctx(), json!({ "count": 50, "dry_run": true })).await;
    assert_eq!(result.unwrap(), json!({ "count": 50 }));
    // serde's own errors still reach the model as invalid arguments.
    let err = LegacyBook.execute(ctx(), json!({})).await.unwrap_err();
    assert_eq!(err.code, adk_tool::TOOL_INVALID_ARGUMENTS_CODE);
}

// === Test 11: Composed argument types are not falsely rejected ===

#[derive(Deserialize, JsonSchema)]
struct Paging {
    page: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Filter {
    ByName { name: String },
    ByAge { age: u32 },
}

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    query: String,
    #[serde(flatten)]
    paging: Paging,
    filter: Option<Filter>,
}

/// Search with paging and an optional filter.
#[tool]
async fn search(args: SearchArgs) -> Result<Value, AdkError> {
    Ok(json!({ "query": args.query, "page": args.paging.page, "filtered": args.filter.is_some() }))
}

#[derive(Deserialize, JsonSchema)]
struct FlatFilterArgs {
    query: String,
    #[serde(flatten)]
    filter: Filter,
}

/// Search with a flattened filter.
#[tool]
async fn flat_search(args: FlatFilterArgs) -> Result<Value, AdkError> {
    let kind = match args.filter {
        Filter::ByName { name } => name,
        Filter::ByAge { age } => age.to_string(),
    };
    Ok(json!({ "query": args.query, "filter": kind }))
}

#[derive(Deserialize, JsonSchema)]
struct OpenArgs {
    query: String,
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

/// Search, keeping any extra keys.
#[tool]
async fn open_search(args: OpenArgs) -> Result<Value, AdkError> {
    Ok(json!({ "query": args.query, "extra": args.extra.len() }))
}

#[tokio::test]
async fn flattened_struct_and_tagged_enum_fields_validate() {
    let ok = json!({ "query": "q", "page": 1, "filter": { "kind": "by_name", "name": "a" } });
    assert!(Search.execute(ctx(), ok).await.is_ok());
    assert!(
        Search.execute(ctx(), json!({ "query": "q", "page": 1, "filter": null })).await.is_ok()
    );

    let err = Search.execute(ctx(), json!({ "query": "q", "page": 1, "bogus": 1 })).await;
    assert_eq!(err.unwrap_err().code, adk_tool::TOOL_INVALID_ARGUMENTS_CODE);
    let nested =
        json!({ "query": "q", "page": 1, "filter": { "kind": "by_age", "age": 3, "x": 1 } });
    assert_eq!(
        Search.execute(ctx(), nested).await.unwrap_err().code,
        adk_tool::TOOL_INVALID_ARGUMENTS_CODE
    );
}

#[tokio::test]
async fn a_flattened_enum_and_a_flattened_map_accept_their_keys() {
    let flat = json!({ "query": "q", "kind": "by_age", "age": 3 });
    assert_eq!(
        FlatSearch.execute(ctx(), flat).await.unwrap(),
        json!({ "query": "q", "filter": "3" })
    );

    let open = json!({ "query": "q", "anything": 1, "more": "x" });
    assert_eq!(OpenSearch.execute(ctx(), open).await.unwrap(), json!({ "query": "q", "extra": 2 }));
}
