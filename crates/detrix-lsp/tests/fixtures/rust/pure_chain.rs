//! Pure function chain fixture for Rust LSP purity testing
//!
//! All functions in this file are pure - they don't have side effects.

/// Calculate total price from quantity and unit price (pure)
/// Calls: compute_subtotal, apply_discount
pub fn calculate_total(quantity: i32, unit_price: f64, discount_percent: f64) -> f64 {
    let subtotal = compute_subtotal(quantity, unit_price);
    let discount = apply_discount(subtotal, discount_percent);
    subtotal - discount
}

/// Compute subtotal from quantity and price (pure)
/// Calls: compute_item_price
pub fn compute_subtotal(quantity: i32, unit_price: f64) -> f64 {
    (0..quantity).map(|_| compute_item_price(unit_price)).sum()
}

/// Compute single item price (pure leaf function)
pub fn compute_item_price(base_price: f64) -> f64 {
    base_price * 1.1 // Add 10% tax
}

/// Apply discount to subtotal (pure)
pub fn apply_discount(subtotal: f64, discount_percent: f64) -> f64 {
    subtotal * (discount_percent / 100.0)
}

/// Format a currency value (pure)
/// Uses only string formatting
pub fn format_currency(amount: f64) -> String {
    format!("${:.2}", amount)
}

/// Calculate average of numbers (pure)
/// Uses iterator methods only
pub fn calculate_average(numbers: &[f64]) -> Option<f64> {
    if numbers.is_empty() {
        None
    } else {
        let sum: f64 = numbers.iter().sum();
        Some(sum / numbers.len() as f64)
    }
}

/// Find maximum value (pure)
/// Uses iterator methods only
pub fn find_max(numbers: &[i32]) -> Option<i32> {
    numbers.iter().copied().max()
}

/// Check if all values are positive (pure)
pub fn all_positive(numbers: &[i32]) -> bool {
    numbers.iter().all(|&n| n > 0)
}

/// Transform values with a pure function (pure)
pub fn double_values(numbers: &[i32]) -> Vec<i32> {
    numbers.iter().map(|&n| n * 2).collect()
}

/// Filter and transform (pure)
pub fn filter_positive_and_double(numbers: &[i32]) -> Vec<i32> {
    numbers
        .iter()
        .filter(|&&n| n > 0)
        .map(|&n| n * 2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_total() {
        let total = calculate_total(5, 10.0, 10.0);
        assert!(total > 0.0);
    }

    #[test]
    fn test_format_currency() {
        assert_eq!(format_currency(123.456), "$123.46");
    }
}
