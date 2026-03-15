//! DeepSeek Thinking Mode with Tool Calls
//!
//! This example demonstrates using DeepSeek's reasoning model (deepseek-reasoner)
//! combined with function calling. The model thinks through problems step-by-step
//! while using tools to gather information.
//!
//! Key features:
//! - Chain-of-thought reasoning visible in output
//! - Tool calls during reasoning process
//! - Multi-step problem solving
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! cargo run --example deepseek_thinking_tools --features deepseek
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_tool::FunctionTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// Arguments for stock price lookup.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct StockPriceArgs {
    /// The stock ticker symbol, e.g. 'AAPL', 'GOOGL', 'MSFT'
    symbol: String,
}

/// Arguments for currency conversion.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CurrencyConvertArgs {
    /// The amount to convert
    amount: f64,
    /// Source currency code (e.g., USD, EUR, JPY)
    from_currency: String,
    /// Target currency code (e.g., USD, EUR, JPY)
    to_currency: String,
}

/// Arguments for financial calculations.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct FinancialCalcArgs {
    /// The calculation to perform
    operation: FinancialOperation,
    /// Array of values for the calculation
    values: Vec<f64>,
    /// Array of quantities (for portfolio calculations)
    #[serde(default)]
    quantities: Vec<f64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FinancialOperation {
    /// Calculate total portfolio value (sum of price * quantity)
    PortfolioValue,
    /// Calculate percentage gain/loss from first to last value
    GainLoss,
    /// Calculate average of values
    AveragePrice,
}

/// Get stock price (mock data).
async fn get_stock_price(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value, adk_core::AdkError> {
    let args: StockPriceArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let symbol = args.symbol.to_uppercase();

    // Mock stock prices
    let (price, change, change_pct) = match symbol.as_str() {
        "AAPL" => (178.52, 2.34, 1.33),
        "GOOGL" => (141.80, -0.95, -0.67),
        "MSFT" => (378.91, 4.12, 1.10),
        "NVDA" => (495.22, 12.50, 2.59),
        "TSLA" => (248.50, -3.20, -1.27),
        "AMZN" => (178.25, 1.80, 1.02),
        _ => (0.0, 0.0, 0.0),
    };

    Ok(json!({
        "symbol": symbol,
        "price": price,
        "change": change,
        "change_percent": change_pct,
        "currency": "USD"
    }))
}

/// Convert currency (mock rates).
async fn convert_currency(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value, adk_core::AdkError> {
    let args: CurrencyConvertArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let from = args.from_currency.to_uppercase();
    let to = args.to_currency.to_uppercase();

    // Mock exchange rates (to USD)
    let to_usd = match from.as_str() {
        "USD" => 1.0,
        "EUR" => 1.09,
        "JPY" => 0.0067,
        "GBP" => 1.27,
        "CNY" => 0.14,
        _ => 1.0,
    };

    let from_usd = match to.as_str() {
        "USD" => 1.0,
        "EUR" => 0.92,
        "JPY" => 149.50,
        "GBP" => 0.79,
        "CNY" => 7.24,
        _ => 1.0,
    };

    let converted = args.amount * to_usd * from_usd;

    Ok(json!({
        "from_amount": args.amount,
        "from_currency": from,
        "to_amount": (converted * 100.0).round() / 100.0,
        "to_currency": to,
        "exchange_rate": (to_usd * from_usd * 10000.0).round() / 10000.0
    }))
}

/// Perform financial calculations.
async fn financial_calculate(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value, adk_core::AdkError> {
    let args: FinancialCalcArgs = serde_json::from_value(args)
        .map_err(|e| adk_core::AdkError::Tool(format!("Invalid args: {}", e)))?;

    let result = match args.operation {
        FinancialOperation::PortfolioValue => {
            // Sum of (price * quantity) for each position
            args.values.iter().zip(args.quantities.iter()).map(|(p, q)| p * q).sum::<f64>()
        }
        FinancialOperation::GainLoss => {
            // Percentage change from first to last value
            if args.values.len() >= 2 {
                let first = args.values[0];
                let last = args.values[args.values.len() - 1];
                if first != 0.0 { ((last - first) / first) * 100.0 } else { 0.0 }
            } else {
                0.0
            }
        }
        FinancialOperation::AveragePrice => {
            if !args.values.is_empty() {
                args.values.iter().sum::<f64>() / args.values.len() as f64
            } else {
                0.0
            }
        }
    };

    Ok(json!({
        "result": (result * 100.0).round() / 100.0
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create DeepSeek Reasoner client (with thinking mode)
    let model = DeepSeekClient::new(DeepSeekConfig::reasoner(api_key))?;

    // Create tools with schemas
    let stock_tool = FunctionTool::new(
        "get_stock_price",
        "Get the current stock price for a given ticker symbol",
        get_stock_price,
    )
    .with_parameters_schema::<StockPriceArgs>();

    let currency_tool = FunctionTool::new(
        "convert_currency",
        "Convert an amount from one currency to another using current exchange rates",
        convert_currency,
    )
    .with_parameters_schema::<CurrencyConvertArgs>();

    let calc_tool = FunctionTool::new(
        "financial_calculate",
        "Perform financial calculations: portfolio value, percentage gain/loss, average price",
        financial_calculate,
    )
    .with_parameters_schema::<FinancialCalcArgs>();

    // Build agent with reasoning and tools
    let agent = LlmAgentBuilder::new("financial_analyst")
        .model(Arc::new(model))
        .instruction(
            "You are a financial analyst assistant. You have access to stock prices, \
             currency conversion, and financial calculation tools. When answering questions, \
             think through the problem step by step, gather the necessary data using tools, \
             and provide a clear analysis.",
        )
        .tool(Arc::new(stock_tool))
        .tool(Arc::new(currency_tool))
        .tool(Arc::new(calc_tool))
        .build()?;

    println!("=== DeepSeek Thinking Mode + Tool Calls Demo ===\n");
    println!("This uses deepseek-reasoner which shows chain-of-thought reasoning.");
    println!("The model will think step-by-step while using tools.\n");
    println!(
        "Try asking: 'I own 50 shares of AAPL and 30 shares of MSFT. What is my portfolio worth?'\n"
    );

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "deepseek_thinking_tools".to_string(),
        "user_1".to_string(),
    )
    .await?;

    Ok(())
}
