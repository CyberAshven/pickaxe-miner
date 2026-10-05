//! Exact fee amount parsing shared across platform transports.
use serde_json::Value;

pub(crate) fn bch_value_to_sats(value: &Value) -> Result<u64, String> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return Err("BCH amount must be numeric".into()),
    };
    decimal_bch_to_sats(&text)
}

pub(crate) fn decimal_bch_to_sats(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('-') || text.starts_with('+') {
        return Err("BCH amount must be non-negative".into());
    }

    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => {
            let exponent = text[index + 1..]
                .parse::<i32>()
                .map_err(|_| "relay fee has an invalid decimal exponent")?;
            (&text[..index], exponent)
        }
        None => (text, 0),
    };
    let (whole, fractional) = match mantissa.split_once('.') {
        Some((whole, fractional)) => {
            if fractional.contains('.') {
                return Err("BCH amount has more than one decimal point".into());
            }
            (whole, fractional)
        }
        None => (mantissa, ""),
    };
    if whole.is_empty() && fractional.is_empty() {
        return Err("BCH amount is empty".into());
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err("BCH amount contains non-decimal characters".into());
    }

    let digits = format!("{whole}{fractional}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let mut value = digits
        .parse::<u128>()
        .map_err(|_| "BCH amount decimal is too large")?;
    let scale = 8i32
        .checked_add(exponent)
        .and_then(|scale| scale.checked_sub(fractional.len() as i32))
        .ok_or("BCH amount scale overflow")?;
    if scale >= 0 {
        let multiplier = 10u128
            .checked_pow(scale as u32)
            .ok_or("BCH amount scale is too large")?;
        value = value
            .checked_mul(multiplier)
            .ok_or("BCH amount satoshi value overflow")?;
    } else {
        let divisor = 10u128
            .checked_pow(scale.unsigned_abs())
            .ok_or("BCH amount scale is too small")?;
        if value % divisor != 0 {
            return Err("BCH amount has precision below one satoshi".into());
        }
        value /= divisor;
    }
    u64::try_from(value).map_err(|_| "BCH amount exceeds u64 satoshis".into())
}
