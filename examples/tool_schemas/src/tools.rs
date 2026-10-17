//! The `#[tool]` tool every scenario uses, and a log of the calls that ran.

use std::sync::Mutex;

use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// The arguments of every `book_slots` call that ran, in order.
pub static BOOKINGS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

pub fn bookings() -> Vec<Value> {
    BOOKINGS.lock().unwrap().clone()
}

pub fn reset_bookings() {
    BOOKINGS.lock().unwrap().clear();
}

/// A postal address.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Address {
    /// Street name and number.
    pub street: String,
    /// City.
    pub city: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BookSlotsArgs {
    /// Room code: one capital letter and one to three digits, such as `A1`.
    #[schemars(length(min = 2, max = 4), pattern(r"^[A-Z][0-9]{1,3}$"))]
    pub room: String,
    /// How many consecutive 30-minute slots to book, from 1 to 10.
    #[schemars(range(min = 1, max = 10))]
    pub count: u32,
    /// Where to post the booking confirmation.
    pub confirm_to: Address,
    /// Attendee email addresses, at most 3.
    #[serde(default)]
    #[schemars(length(max = 3))]
    pub attendees: Vec<String>,
}

/// Books consecutive 30-minute slots in a meeting room and posts a confirmation.
#[tool]
pub async fn book_slots(args: BookSlotsArgs) -> Result<Value, AdkError> {
    let booking = json!({
        "room": args.room,
        "count": args.count,
        "street": args.confirm_to.street,
        "city": args.confirm_to.city,
        "attendees": args.attendees.len(),
    });
    BOOKINGS.lock().unwrap().push(booking.clone());
    Ok(json!({ "booked": true, "booking": booking }))
}
