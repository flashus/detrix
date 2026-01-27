//! Mixed purity function chain fixture for Rust LSP purity testing
//!
//! Contains both pure and impure functions to test transitive analysis.

/// Analyze data (mixed - eventually calls println which is acceptable impurity)
/// Calls: process_items, log_result
pub fn analyze_data(items: &[f64]) -> f64 {
    let processed = process_items(items);
    log_result(processed);
    processed
}

/// Process items (pure)
/// Calls: compute_stats
pub fn process_items(items: &[f64]) -> f64 {
    compute_stats(items)
}

/// Compute statistics (pure)
/// Uses only pure operations
pub fn compute_stats(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let sum: f64 = values.iter().sum();
    let avg = sum / values.len() as f64;

    // Compute variance
    let variance: f64 = values
        .iter()
        .map(|&x| {
            let diff = x - avg;
            diff * diff
        })
        .sum::<f64>() / values.len() as f64;

    variance.sqrt() // Standard deviation
}

/// Log result (acceptable impurity - println)
pub fn log_result(value: f64) {
    println!("Result: {}", value);
}

/// Transform and filter (pure)
pub fn transform_data(items: &[i32]) -> Vec<i32> {
    items
        .iter()
        .filter(|&&x| x > 0)
        .map(|&x| x * 2)
        .collect()
}

/// Sort values (pure - creates new sorted vec)
pub fn sort_values(items: &[i32]) -> Vec<i32> {
    let mut sorted = items.to_vec();
    sorted.sort();
    sorted
}

/// Find median (pure)
pub fn find_median(items: &[i32]) -> Option<f64> {
    if items.is_empty() {
        return None;
    }

    let mut sorted = items.to_vec();
    sorted.sort();

    let len = sorted.len();
    if len % 2 == 0 {
        let mid = len / 2;
        Some((sorted[mid - 1] + sorted[mid]) as f64 / 2.0)
    } else {
        Some(sorted[len / 2] as f64)
    }
}

/// Chain with file I/O (impure)
/// Calls: read_data_file (which does file I/O)
pub fn process_from_file(path: &str) -> Result<f64, String> {
    let data = read_data_file(path)?;
    Ok(compute_stats(&data))
}

/// Read data from file (impure - file I/O)
fn read_data_file(path: &str) -> Result<Vec<f64>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    content
        .lines()
        .map(|line| line.parse::<f64>().map_err(|e| e.to_string()))
        .collect()
}

/// Pure computation with Result handling
pub fn safe_divide(a: f64, b: f64) -> Option<f64> {
    if b == 0.0 {
        None
    } else {
        Some(a / b)
    }
}

/// Chain pure computations with Option
pub fn compute_ratio(values: &[f64]) -> Option<f64> {
    let first = values.first().copied()?;
    let last = values.last().copied()?;
    safe_divide(first, last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_stats() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let std_dev = compute_stats(&values);
        assert!(std_dev > 0.0);
    }

    #[test]
    fn test_find_median() {
        assert_eq!(find_median(&[1, 2, 3, 4, 5]), Some(3.0));
        assert_eq!(find_median(&[1, 2, 3, 4]), Some(2.5));
    }
}
