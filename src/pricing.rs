// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! What a model's tokens cost. Claude Code records token usage per turn but never a price, so
//! cost is computed from a snapshot of published rates. An unknown model reports tokens with
//! no cost rather than guessing a number that would look authoritative and be wrong.
//!
//! This table necessarily names specific Claude and GPT models to price their tokens — a
//! different concern from the launch path, which pins none (spec, *Settled decisions*: "the core pins
//! no model"). Pricing a model it is told about is not choosing one.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read_multiple: f64,
}

const CACHE_WRITE_5M: f64 = 1.25;
const CACHE_WRITE_1H: f64 = 2.0;
const CACHE_READ: f64 = 0.1;
/// Opus 5.5 and Sonnet 5.5 read cached tokens at half the usual multiple.
const CACHE_READ_5_5: f64 = 0.05;

/// Published standard-tier rates, per million tokens. Matching is by prefix so a dated
/// snapshot id prices like its family; a more specific prefix is listed before a shorter one
/// it shares a start with.
///
/// Claude: Anthropic's rates as of 2026-10-23 (platform.claude.com/docs/en/about-claude/pricing).
/// GPT: OpenAI's short-context rates as of 2026-09
/// (developers.openai.com/api/docs/pricing), where a cache write costs 1.25x input and a
/// cached read 0.1x, as Claude's five-minute cache does. Codex caps a prompt below the
/// long-context tier, so that tier is not priced. A Codex model with no published API rate —
/// `codex-auto-review`, the model its guardian reviews run on — prices as unknown.
pub fn rates(model: &str) -> Option<Rates> {
    let table: [(&str, f64, f64, f64); 13] = [
        ("claude-opus-5-5", 4.0, 20.0, CACHE_READ_5_5),
        ("claude-opus", 5.0, 25.0, CACHE_READ),
        ("claude-sonnet-5-5", 2.0, 10.0, CACHE_READ_5_5),
        ("claude-sonnet-5", 2.0, 10.0, CACHE_READ),
        ("claude-sonnet-4", 3.0, 15.0, CACHE_READ),
        ("claude-haiku-4", 1.0, 5.0, CACHE_READ),
        ("gpt-6-astra", 10.0, 50.0, CACHE_READ),
        ("gpt-6-sol", 2.0, 10.0, CACHE_READ),
        ("gpt-6-luna", 0.10, 0.50, CACHE_READ),
        ("gpt-5.6-sol", 4.0, 20.0, CACHE_READ),
        ("gpt-5.6-terra", 2.0, 12.0, CACHE_READ),
        ("gpt-5.6-luna", 0.20, 1.20, CACHE_READ),
        ("gpt-5.5", 5.0, 30.0, CACHE_READ),
    ];
    table
        .iter()
        .find(|(id, _, _, _)| model.starts_with(id))
        .map(|(_, input, output, read)| Rates {
            input: *input,
            output: *output,
            cache_read_multiple: *read,
        })
}

const MILLION: u64 = 1_000_000;
const TWO_HUNDRED_K: u64 = 200_000;
const CODEX_WINDOW: u64 = 272_000;

/// How much context a model holds, for turning a turn's input tokens into the share of the
/// window they occupy.
pub fn context_window(model: &str) -> Option<u64> {
    let table: [(&str, u64); 8] = [
        ("claude-opus-5", MILLION),
        ("claude-sonnet-5", MILLION),
        ("claude-sonnet-4", TWO_HUNDRED_K),
        ("claude-opus-4", TWO_HUNDRED_K),
        ("claude-haiku", TWO_HUNDRED_K),
        ("gpt-6-", CODEX_WINDOW),
        ("gpt-5.6-", CODEX_WINDOW),
        ("gpt-5.5", CODEX_WINDOW),
    ];
    table
        .iter()
        .find(|(id, _)| model.starts_with(id))
        .map(|(_, window)| *window)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_read += other.cache_read;
    }

    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn cost(&self, model: &str) -> Option<f64> {
        let rates = rates(model)?;
        let per_token = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;
        Some(
            per_token(self.input, rates.input)
                + per_token(self.output, rates.output)
                + per_token(self.cache_write_5m, rates.input * CACHE_WRITE_5M)
                + per_token(self.cache_write_1h, rates.input * CACHE_WRITE_1H)
                + per_token(self.cache_read, rates.input * rates.cache_read_multiple),
        )
    }
}

/// What a session spent, per model.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub by_model: Vec<(String, Tokens)>,
}

impl Usage {
    pub fn add(&mut self, model: &str, tokens: &Tokens) {
        if let Some((_, existing)) = self.by_model.iter_mut().find(|(m, _)| m == model) {
            existing.add(tokens);
        } else {
            self.by_model.push((model.to_string(), *tokens));
        }
    }

    pub fn tokens(&self) -> Tokens {
        let mut total = Tokens::default();
        for (_, tokens) in &self.by_model {
            total.add(tokens);
        }
        total
    }

    pub fn merge(&mut self, other: &Usage) {
        for (model, tokens) in &other.by_model {
            self.add(model, tokens);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_model.iter().all(|(_, tokens)| tokens.is_empty())
    }

    pub fn cost(&self) -> Option<f64> {
        let mut total = 0.0;
        let mut any = false;
        for (model, tokens) in &self.by_model {
            if let Some(cost) = tokens.cost(model) {
                total += cost;
                any = true;
            }
        }
        any.then_some(total)
    }
}

/// Token counts at a glance: `934k`, `1.2M`.
pub fn human_tokens(tokens: u64) -> String {
    match tokens {
        0 => "-".to_string(),
        1..=9_999 => format!("{tokens}"),
        10_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// Money at a glance, never rounded to a misleading zero.
pub fn human_money(dollars: f64) -> String {
    if dollars >= 100.0 {
        format!("${dollars:.0}")
    } else if dollars >= 1.0 {
        format!("${dollars:.2}")
    } else if dollars > 0.0 {
        format!("${dollars:.3}")
    } else {
        "$0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_tokens_per_model_and_prices_a_known_one() {
        let mut usage = Usage::default();
        usage.add(
            "claude-sonnet-5",
            &Tokens {
                input: 2,
                output: 7,
                cache_write_5m: 100,
                cache_write_1h: 0,
                cache_read: 1_000,
            },
        );
        usage.add(
            "claude-sonnet-5",
            &Tokens {
                input: 3,
                output: 11,
                ..Tokens::default()
            },
        );
        usage.add(
            "some-unknown-model",
            &Tokens {
                input: 5,
                ..Tokens::default()
            },
        );
        assert_eq!(usage.by_model.len(), 2);
        let tokens = usage.tokens();
        assert_eq!(tokens.input, 10);
        assert_eq!(tokens.output, 18);
        assert!(usage.cost().is_some());
    }

    #[test]
    fn a_gpt_model_prices_by_its_own_rates_and_an_unpublished_one_as_none() {
        let tokens = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 1_000_000,
        };
        let cost = |model: &str| tokens.cost(model).unwrap();
        assert!((cost("gpt-6-sol") - 12.2).abs() < 1e-9);
        assert!((cost("gpt-5.6-terra") - 14.2).abs() < 1e-9);
        assert!((cost("gpt-5.5") - 35.5).abs() < 1e-9);
        assert!(rates("codex-auto-review").is_none());
        assert!(rates("gpt-4o").is_none());
    }

    #[test]
    fn opus_and_sonnet_5_5_price_by_their_own_rates_not_their_family_prefix() {
        let tokens = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 0,
            cache_read: 1_000_000,
        };
        let cost = |model: &str| tokens.cost(model).unwrap();
        // input + output + 5-minute cache write (1.25x input) + cache read
        assert!((cost("claude-opus-5-5") - (4.0 + 20.0 + 5.0 + 0.2)).abs() < 1e-9);
        assert!((cost("claude-sonnet-5-5") - (2.0 + 10.0 + 2.5 + 0.1)).abs() < 1e-9);
        assert!((cost("claude-opus-5") - (5.0 + 25.0 + 6.25 + 0.5)).abs() < 1e-9);
        assert!((cost("claude-sonnet-5") - (2.0 + 10.0 + 2.5 + 0.2)).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_model_prices_as_none() {
        assert!(rates("some-unknown-model").is_none());
        assert!(Tokens::default().cost("some-unknown-model").is_none());
    }
}
