//! The weekly review page and the summary sent to the owner's channel (spec 3.5 §7.21).
//!
//! The review holds KPIs against baselines and targets with trends, what each agent did,
//! cost, revenue and profit and loss, experiments, incidents, leads handed off, promotion
//! offers and demotions, alarms that waited, and a short list of questions. Reviews come from
//! the weekly-review job ([`WebsiteDesk::record_review`]) or are composed here; sending one
//! puts its one-line summary on the owner's channel and records `website.review.sent`.

use adk_core::Result;
use adk_kernel::charter::{KpiPhase, ladder_rank};
use awp_types::Role;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::site::KpiCard;
use super::words::{self, major};
use super::{Line, Pill, Tone, WebsiteDesk, invalid, not_found, unavailable};
use crate::review::{ReviewInputs, Trend, WeeklyReview};

/// Where and when a review's summary went.
type Sent = (String, DateTime<Utc>);

#[derive(Default)]
pub(crate) struct State {
    reviews: Vec<(WeeklyReview, Option<Sent>)>,
}

/// One review in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewLine {
    /// The Monday its week starts.
    pub week: NaiveDate,
    /// Its title.
    pub title: String,
    /// Where and when its summary went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<String>,
}

/// What each agent did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkLine {
    /// The agent.
    pub agent: String,
    /// What it did.
    pub items: Vec<String>,
}

/// Cost, revenue and profit and loss (spec 3.5 §7.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoneyView {
    /// Spent this week.
    pub spend: String,
    /// Spent this month, against the budget.
    pub month: String,
    /// Whether the month is inside the budget.
    pub within: Pill,
    /// Each category this week.
    pub categories: Vec<Line>,
    /// Revenue, profit, cost per lead and per sale, where the site sells.
    pub results: Vec<Line>,
}

/// A promotion the jobs earned, for the owner to decide (A-I13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferLine {
    /// The job.
    pub job: String,
    /// What it is.
    pub title: String,
    /// Its level now.
    pub from: String,
    /// The level a promotion gives it.
    pub to: String,
    /// The record behind the offer.
    pub record: String,
}

/// One week's review (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewView {
    /// The Monday its week starts.
    pub week: NaiveDate,
    /// "Week of 19 October".
    pub title: String,
    /// The charter version in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter_version: Option<u32>,
    /// The week at a glance.
    pub headline: Vec<Line>,
    /// KPIs against baselines and targets, with trends.
    pub kpis: Vec<KpiCard>,
    /// What each agent did.
    pub work: Vec<WorkLine>,
    /// Money.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub money: Option<MoneyView>,
    /// Experiments and their evidence.
    pub experiments: Vec<String>,
    /// Incidents.
    pub incidents: Vec<String>,
    /// Leads captured and handed off.
    pub leads: Vec<String>,
    /// Promotions the jobs earned.
    pub promotions: Vec<OfferLine>,
    /// Demotions, with why.
    pub demotions: Vec<Line>,
    /// Alarms that waited for the review.
    pub alarms: Vec<String>,
    /// Questions for the owner.
    pub questions: Vec<String>,
    /// What the constant-update engine did: updates, deliveries, what it held and its cost;
    /// empty when the site runs no engine (spec 3.5 §7.8.3).
    pub updates: Vec<Line>,
    /// The line sent to the owner's channel.
    pub summary: String,
    /// The review as a document, in Markdown.
    pub markdown: String,
    /// Where and when its summary went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<String>,
}

/// The weekly review page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPage {
    /// Every review, newest first.
    pub reviews: Vec<ReviewLine>,
    /// The review shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<ReviewView>,
    /// Whether the viewer can compose one now.
    pub can_compose: bool,
    /// The owner's channel the summary goes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// When the next one is due.
    pub next_due: NaiveDate,
}

/// A review action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReviewCommand {
    /// Composes the review of the week starting `week`, last week's when absent.
    Compose {
        /// The Monday.
        #[serde(default)]
        week: Option<NaiveDate>,
    },
    /// Sends the review's summary to the owner's channel.
    Send {
        /// The Monday of the review.
        week: NaiveDate,
    },
}

fn monday(day: NaiveDate) -> NaiveDate {
    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

fn sent_words(sent: Option<&Sent>) -> Option<String> {
    sent.map(|(channel, at)| {
        let channel = match channel.as_str() {
            "whatsapp" => "WhatsApp",
            "telegram" => "Telegram",
            "sms" => "text message",
            "slack" => "Slack",
            "discord" => "Discord",
            "matrix" => "Matrix",
            other => other,
        };
        format!("Sent to {channel} on {} UTC", at.format("%-d %b at %H:%M"))
    })
}

pub(crate) fn review_view(
    review: &WeeklyReview,
    sent: Option<&Sent>,
    currency: &str,
) -> ReviewView {
    let up = review.kpis.iter().filter(|k| k.trend == Trend::Up).count();
    let mut headline = vec![Line::new("Measures better", format!("{up} of {}", review.kpis.len()))];
    if let Some(money) = &review.money {
        headline.push(Line::new("Spent this week", major(money.spend, currency)));
    }
    if let Some(leads) = review.leads {
        headline.push(Line::new("Leads", leads.to_string()));
    }
    headline.push(Line::new("Incidents", review.incidents.len().to_string()));
    headline.push(Line::new("Questions for you", review.questions.len().to_string()));
    let kpis = review
        .kpis
        .iter()
        .map(|line| {
            let k = &line.review;
            KpiCard {
                id: k.kpi.clone(),
                name: k.name.clone(),
                this_week: k.week.map(words::number),
                last_week: k.previous_week.map(words::number),
                baseline: k.baseline.map(words::number),
                target: k.target.map(words::number),
                trend: match line.trend {
                    Trend::Up => Pill::new("Better", Tone::Good),
                    Trend::Down => Pill::new("Worse", Tone::Bad),
                    Trend::Flat => Pill::new("Same", Tone::Neutral),
                    Trend::Unknown => Pill::new("New", Tone::Neutral),
                },
                phase: match &k.phase {
                    KpiPhase::MeasureOnly { until } => {
                        format!("Finding its baseline until {}", until.format("%-d %b"))
                    }
                    _ => "Tracking".into(),
                },
            }
        })
        .collect();
    let work = review
        .work
        .iter()
        .map(|(agent, items)| WorkLine { agent: words::agent(*agent).into(), items: items.clone() })
        .collect();
    let money = review.money.as_ref().map(|money| {
        let mut results = Vec::new();
        if let Some(revenue) = money.revenue {
            results.push(Line::new("Revenue", major(revenue, currency)));
        }
        if let Some(profit) = money.profit {
            results.push(Line::new("Profit", major(profit, currency)));
        }
        if let Some(per_lead) = money.cost_per_lead {
            results.push(Line::new("Cost per lead", major(per_lead, currency)));
        }
        if let Some(per_sale) = money.cost_per_sale {
            results.push(Line::new("Cost per sale", major(per_sale, currency)));
        }
        MoneyView {
            spend: major(money.spend, currency),
            month: format!(
                "{} of {}",
                major(money.month_to_date, currency),
                major(money.monthly_budget, currency)
            ),
            within: if money.within_budget {
                Pill::new("Within budget", Tone::Good)
            } else {
                Pill::new("Over budget", Tone::Bad)
            },
            categories: money
                .by_category
                .iter()
                .map(|(category, spent)| {
                    let mut name = category.clone();
                    if let Some(first) = name.get_mut(..1) {
                        first.make_ascii_uppercase();
                    }
                    Line::new(name, major(*spent, currency))
                })
                .collect(),
            results,
        }
    });
    let mut leads = Vec::new();
    if let Some(count) = review.leads {
        leads.push(format!("{count} captured this week"));
    }
    leads.extend(review.handed_off.iter().map(|lead| format!("Handed to you: {lead}")));
    ReviewView {
        week: review.week_start,
        title: format!("Week of {}", review.week_start.format("%-d %B")),
        charter_version: review.charter_version,
        headline,
        kpis,
        work,
        money,
        experiments: review.experiments.clone(),
        incidents: review
            .incidents
            .iter()
            .map(|incident| {
                format!(
                    "{} file{} changed on the host; {} to release {}",
                    incident.drift.len(),
                    if incident.drift.len() == 1 { "" } else { "s" },
                    if incident.closed.is_some() { "rolled back" } else { "rolling back" },
                    incident.rollback.to_release
                )
            })
            .collect(),
        leads,
        promotions: review
            .promotions
            .iter()
            .filter(|offer| offer.eligible)
            .map(|offer| OfferLine {
                job: offer.job.clone(),
                title: crate::jobs::job(&offer.job)
                    .map_or_else(|| offer.job.clone(), |s| s.title.to_string()),
                from: words::level(offer.level).into(),
                to: words::level(offer.next.unwrap_or(offer.level)).into(),
                record: format!("{} proposals in a row approved unchanged", offer.streak),
            })
            .collect(),
        demotions: review
            .demotions
            .iter()
            .filter(|change| {
                change.from.is_some_and(|from| ladder_rank(change.to) < ladder_rank(from))
            })
            .map(|change| {
                Line::new(
                    crate::jobs::job(&change.job)
                        .map_or_else(|| change.job.clone(), |s| s.title.to_string()),
                    format!(
                        "Lowered to \u{201c}{}\u{201d}: {}",
                        words::level(change.to),
                        change.reason.describe()
                    ),
                )
            })
            .collect(),
        alarms: review.alarms.iter().map(|alarm| alarm.summary.clone()).collect(),
        questions: review.questions.clone(),
        updates: review
            .updates
            .as_ref()
            .map(|u| {
                vec![
                    Line::new(
                        "Updates",
                        format!(
                            "{} ({} live, {} in releases)",
                            u.updates, u.live_updates, u.release_updates
                        ),
                    ),
                    Line::new("Releases", u.releases.to_string()),
                    Line::new("Written with a model", u.updates_with_model.to_string()),
                    Line::new(
                        "Feed deliveries",
                        format!("{}, {} held whole", u.deliveries, u.held_deliveries),
                    ),
                    Line::new("Items held alone", u.held_items.to_string()),
                    Line::new(
                        "Cost per thousand updates",
                        major(u.cost_per_thousand_updates_micros / 1_000_000.0, currency),
                    ),
                ]
            })
            .unwrap_or_default(),
        summary: review.summary(),
        markdown: review.to_markdown(),
        sent: sent_words(sent),
    }
}

impl WebsiteDesk {
    /// Keeps a review the weekly-review job composed, so the web home shows it (spec 3.5
    /// §7.21). A review of the same week replaces the earlier one.
    pub async fn record_review(&self, review: WeeklyReview) {
        let mut state = self.inner.reviews.lock().await;
        state.reviews.retain(|(held, _)| held.week_start != review.week_start);
        state.reviews.push((review, None));
        state.reviews.sort_by_key(|(held, _)| held.week_start);
    }

    /// The weekly review page for `person`, showing the week starting `week`, or the latest.
    ///
    /// # Errors
    ///
    /// `Forbidden` for a person with no role.
    pub async fn review_page(&self, person: &str, week: Option<NaiveDate>) -> Result<ReviewPage> {
        let roles = self.roles(person).await?;
        if roles.is_empty() {
            return Err(super::forbidden(
                "website.ui.not_allowed",
                format!("{person} holds no role in this site's charter"),
            ));
        }
        let state = self.inner.reviews.lock().await;
        let current = match week {
            Some(week) => state.reviews.iter().find(|(r, _)| r.week_start == week),
            None => state.reviews.last(),
        };
        let today = self.now().date_naive();
        Ok(ReviewPage {
            reviews: state
                .reviews
                .iter()
                .rev()
                .map(|(review, sent)| ReviewLine {
                    week: review.week_start,
                    title: format!("Week of {}", review.week_start.format("%-d %B")),
                    sent: sent_words(sent.as_ref()),
                })
                .collect(),
            current: current
                .map(|(review, sent)| review_view(review, sent.as_ref(), &self.inner.currency)),
            can_compose: roles.contains(&Role::Owner) && self.inner.composer.is_some(),
            channel: self.inner.channel.as_ref().map(|(sink, _)| sink.channel().to_string()),
            next_due: monday(today) + Duration::days(7),
        })
    }

    /// Carries out a review `command` for `person` (spec 3.5 §7.21).
    ///
    /// # Errors
    ///
    /// `Forbidden` for a person who is not an owner; `NotFound` for a week with no review;
    /// the composer's and the channel's errors.
    pub async fn review_command(&self, person: &str, command: ReviewCommand) -> Result<ReviewPage> {
        self.require(person, &[Role::Owner], "The weekly review").await?;
        let composer = self
            .inner
            .composer
            .as_ref()
            .ok_or_else(|| unavailable("The weekly review's composer"))?;
        match command {
            ReviewCommand::Compose { week } => {
                let week =
                    monday(week.unwrap_or_else(|| self.now().date_naive() - Duration::days(7)));
                let inputs = match &self.inner.notes {
                    Some(notes) => notes.notes(week).await?,
                    None => ReviewInputs::default(),
                };
                let review = composer.compose(week, inputs).await?;
                self.record_review(review).await;
                self.review_page(person, Some(week)).await
            }
            ReviewCommand::Send { week } => {
                let (sink, thread) =
                    self.inner.channel.clone().ok_or_else(|| unavailable("The owner's channel"))?;
                let review = {
                    let state = self.inner.reviews.lock().await;
                    state
                        .reviews
                        .iter()
                        .find(|(r, _)| r.week_start == week)
                        .map(|(r, _)| r.clone())
                        .ok_or_else(|| {
                            not_found(
                                "website.review.week",
                                format!("no review of the week of {week}"),
                            )
                        })?
                };
                if review.week_start > self.now().date_naive() {
                    return Err(invalid("website.review.future", "that week has not happened yet"));
                }
                composer.send(&review, sink.as_ref(), &thread).await?;
                let mut state = self.inner.reviews.lock().await;
                if let Some((_, sent)) =
                    state.reviews.iter_mut().find(|(r, _)| r.week_start == week)
                {
                    *sent = Some((sink.channel().to_string(), self.now()));
                }
                drop(state);
                self.review_page(person, Some(week)).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_week_starts_on_monday() {
        let sunday = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(monday(sunday), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    }

    #[test]
    fn a_review_reads_in_plain_words() {
        let review = WeeklyReview {
            site: uuid::Uuid::nil(),
            week_start: NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
            charter_version: Some(1),
            kpis: Vec::new(),
            work: Default::default(),
            money: None,
            experiments: Vec::new(),
            incidents: Vec::new(),
            leads: Some(4),
            handed_off: vec!["A catering enquiry for 40 people".into()],
            promotions: Vec::new(),
            demotions: Vec::new(),
            alarms: Vec::new(),
            questions: vec!["Should the site answer in Swahili too?".into()],
            updates: None,
        };
        let view = review_view(&review, Some(&("whatsapp".into(), Utc::now())), "KES");
        assert_eq!(view.title, "Week of 19 October");
        assert_eq!(
            view.leads,
            ["4 captured this week", "Handed to you: A catering enquiry for 40 people"]
        );
        assert!(view.sent.unwrap().starts_with("Sent to WhatsApp"));
        assert_eq!(view.summary, review.summary());
    }
}
