//! Port of upstream/apps/web/src/components/usage/usagePriceForm.ts

use turin_contracts::settings::{UsageModelPriceOverride, UsageModelTokenPrice};

/// One editable rate column (`USAGE_PRICE_FIELDS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UsagePriceField {
    Input,
    Output,
    CacheRead,
    CacheWrite,
}

pub struct UsagePriceFieldSpec {
    pub field: UsagePriceField,
    /// The settings key (`inputCostPerMillionTokens`, …).
    pub key: &'static str,
    pub label: &'static str,
    pub optional: bool,
}

pub const USAGE_PRICE_FIELDS: [UsagePriceFieldSpec; 4] = [
    UsagePriceFieldSpec {
        field: UsagePriceField::Input,
        key: "inputCostPerMillionTokens",
        label: "Input",
        optional: false,
    },
    UsagePriceFieldSpec {
        field: UsagePriceField::Output,
        key: "outputCostPerMillionTokens",
        label: "Output",
        optional: false,
    },
    UsagePriceFieldSpec {
        field: UsagePriceField::CacheRead,
        key: "cacheReadCostPerMillionTokens",
        label: "Cache read",
        optional: true,
    },
    UsagePriceFieldSpec {
        field: UsagePriceField::CacheWrite,
        key: "cacheWriteCostPerMillionTokens",
        label: "Cache write",
        optional: true,
    },
];

impl UsagePriceField {
    pub fn spec(self) -> &'static UsagePriceFieldSpec {
        USAGE_PRICE_FIELDS
            .iter()
            .find(|spec| spec.field == self)
            .expect("every field has a spec")
    }

    fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Output => 1,
            Self::CacheRead => 2,
            Self::CacheWrite => 3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsagePriceForm {
    pub model: String,
    values: [String; 4],
}

impl UsagePriceForm {
    pub fn get(&self, field: UsagePriceField) -> &str {
        &self.values[field.index()]
    }

    pub fn set(&mut self, field: UsagePriceField, value: impl Into<String>) {
        self.values[field.index()] = value.into();
    }
}

/// ECMAScript `Number.prototype.toString` for a finite number: the shortest
/// round-trip digits, exponent form outside `1e-7 < |x| < 1e21`. Rust's
/// `Display` never switches to exponent form, so `1e-7` would print as
/// `0.0000001` and drift from what the web app shows when editing a price.
pub fn js_number_to_string(value: f64) -> String {
    if value == 0. {
        return "0".to_owned();
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN".to_owned()
        } else if value > 0. {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    let sign = if value < 0. { "-" } else { "" };
    // `{:e}` gives the shortest round-trip digits as `d.ddde±x`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("LowerExp has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // value = 0.<digits> × 10^n
    let n = exponent.parse::<i32>().expect("integer exponent") + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let exp = n - 1;
        let exp_sign = if exp < 0 { "-" } else { "+" };
        if k == 1 {
            format!("{digits}e{exp_sign}{}", exp.abs())
        } else {
            format!("{}.{}e{exp_sign}{}", &digits[..1], &digits[1..], exp.abs())
        }
    };
    format!("{sign}{body}")
}

fn price_rate(value: f64) -> UsageModelTokenPrice {
    UsageModelTokenPrice::new(value).expect("form rates are finite and non-negative")
}

/// `usagePriceForm(model, price)`.
pub fn usage_price_form(model: &str, price: Option<&UsageModelPriceOverride>) -> UsagePriceForm {
    let mut form = UsagePriceForm {
        model: model.to_owned(),
        ..Default::default()
    };
    if let Some(price) = price {
        let text = |rate: UsageModelTokenPrice| js_number_to_string(rate.get());
        form.set(
            UsagePriceField::Input,
            text(price.input_cost_per_million_tokens),
        );
        form.set(
            UsagePriceField::Output,
            text(price.output_cost_per_million_tokens),
        );
        if let Some(rate) = price.cache_read_cost_per_million_tokens {
            form.set(UsagePriceField::CacheRead, text(rate));
        }
        if let Some(rate) = price.cache_write_cost_per_million_tokens {
            form.set(UsagePriceField::CacheWrite, text(rate));
        }
    }
    form
}

/// `/^(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i`.
fn is_plain_decimal(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut index = 0;
    let digits = |index: &mut usize| {
        let start = *index;
        while bytes.get(*index).is_some_and(u8::is_ascii_digit) {
            *index += 1;
        }
        *index - start
    };
    let int_digits = digits(&mut index);
    if int_digits > 0 {
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            digits(&mut index);
        }
    } else if bytes.get(index) == Some(&b'.') {
        index += 1;
        if digits(&mut index) == 0 {
            return false;
        }
    } else {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if digits(&mut index) == 0 {
            return false;
        }
    }
    index == bytes.len()
}

/// Blank cache prices use the input rate; explicit zero means free.
pub fn parse_usage_price_form(form: &UsagePriceForm) -> Option<(String, UsageModelPriceOverride)> {
    let model = turin_contracts::base_schemas::js_trim(&form.model).to_owned();
    if model.is_empty() {
        return None;
    }
    let mut rates: [Option<f64>; 4] = [None; 4];
    for spec in &USAGE_PRICE_FIELDS {
        let raw = turin_contracts::base_schemas::js_trim(form.get(spec.field));
        if raw.is_empty() {
            if spec.optional {
                continue;
            }
            return None;
        }
        if !is_plain_decimal(raw) {
            return None;
        }
        let value: f64 = raw.parse().ok()?;
        if !value.is_finite() || value < 0. {
            return None;
        }
        rates[spec.field.index()] = Some(value);
    }
    let input = rates[UsagePriceField::Input.index()]?;
    let output = rates[UsagePriceField::Output.index()]?;
    Some((
        model,
        UsageModelPriceOverride {
            input_cost_per_million_tokens: price_rate(input),
            output_cost_per_million_tokens: price_rate(output),
            cache_read_cost_per_million_tokens: rates[UsagePriceField::CacheRead.index()]
                .map(price_rate),
            cache_write_cost_per_million_tokens: rates[UsagePriceField::CacheWrite.index()]
                .map(price_rate),
        },
    ))
}

/// Test and mock helper: a price with only the required rates.
pub fn basic_price(input: f64, output: f64) -> UsageModelPriceOverride {
    UsageModelPriceOverride {
        input_cost_per_million_tokens: price_rate(input),
        output_cost_per_million_tokens: price_rate(output),
        cache_read_cost_per_million_tokens: None,
        cache_write_cost_per_million_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> UsagePriceForm {
        usage_price_form("example-model", Some(&basic_price(2., 8.)))
    }

    #[test]
    fn preserves_exact_model_ids_and_fractional_rates() {
        let mut form = usage_price_form("", None);
        form.model = "  vendor/Example-Model  ".to_owned();
        form.set(UsagePriceField::Input, "2.5");
        form.set(UsagePriceField::Output, "10");
        assert_eq!(
            parse_usage_price_form(&form),
            Some(("vendor/Example-Model".to_owned(), basic_price(2.5, 10.)))
        );
    }

    #[test]
    fn keeps_free_cache_tokens_distinct_from_blank_cache_prices() {
        let mut form = usage_price_form("example-model", None);
        form.set(UsagePriceField::Input, "2");
        form.set(UsagePriceField::Output, "8");
        form.set(UsagePriceField::CacheRead, "0");
        form.set(UsagePriceField::CacheWrite, " ");
        let (_, price) = parse_usage_price_form(&form).unwrap();
        assert_eq!(
            price,
            UsageModelPriceOverride {
                cache_read_cost_per_million_tokens: Some(price_rate(0.)),
                ..basic_price(2., 8.)
            }
        );
    }

    #[test]
    fn preserves_all_prices_when_editing_including_small_fractional_prices() {
        let price = UsageModelPriceOverride {
            input_cost_per_million_tokens: price_rate(0.),
            output_cost_per_million_tokens: price_rate(8.),
            cache_read_cost_per_million_tokens: Some(price_rate(0.000_000_1)),
            cache_write_cost_per_million_tokens: Some(price_rate(3.5)),
        };
        let form = usage_price_form("example-model", Some(&price));
        // JavaScript prints 1e-7 in exponent form; the form shows the same text.
        assert_eq!(form.get(UsagePriceField::CacheRead), "1e-7");
        assert_eq!(
            parse_usage_price_form(&form),
            Some(("example-model".to_owned(), price))
        );
    }

    #[test]
    fn rejects_invalid_required_prices() {
        for input in ["", " ", "-1", "Infinity", "NaN", "1e999", "$2", "0x10"] {
            let mut form = base();
            form.set(UsagePriceField::Input, input);
            assert_eq!(parse_usage_price_form(&form), None, "{input:?}");
        }
    }

    #[test]
    fn rejects_invalid_optional_rates_and_missing_model_ids() {
        let mut form = base();
        form.set(UsagePriceField::CacheRead, "-1");
        assert_eq!(parse_usage_price_form(&form), None);
        let mut form = base();
        form.set(UsagePriceField::CacheWrite, "invalid");
        assert_eq!(parse_usage_price_form(&form), None);
        let mut form = base();
        form.model = " ".to_owned();
        assert_eq!(parse_usage_price_form(&form), None);
    }

    #[test]
    fn number_text_follows_ecmascript() {
        for (value, text) in [
            (0., "0"),
            (2., "2"),
            (2.5, "2.5"),
            (0.1, "0.1"),
            (0.000_001, "0.000001"),
            (0.000_000_1, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123_456_789., "123456789"),
            (1e21, "1e+21"),
            (1.25e21, "1.25e+21"),
            (1e20, "100000000000000000000"),
            (-3.5, "-3.5"),
        ] {
            assert_eq!(js_number_to_string(value), text, "{value:e}");
        }
    }
}
