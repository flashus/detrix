//! Expression analysis utilities
//!
//! Pure string parsing functions for analyzing expression syntax.
//! No dependencies on external crates - just string operations.

/// Check if expression contains a function call.
///
/// Used to determine whether to use breakpoint mode for Go expressions.
/// Go/Delve requires the `call` prefix for function calls in evaluate requests.
///
/// **WARNING:** Function calls in Go BLOCK the target process while executing.
/// Use simple variable expressions when possible for non-blocking observability.
///
/// Heuristic: look for `identifier(` pattern which indicates function calls.
///
/// # Examples
///
/// ```
/// use detrix_core::expression_contains_function_call;
///
/// // Function calls
/// assert!(expression_contains_function_call("len(x)"));
/// assert!(expression_contains_function_call("fmt.Sprintf(\"test\")"));
/// assert!(expression_contains_function_call("user.GetName()"));
///
/// // Not function calls
/// assert!(!expression_contains_function_call("user.name"));
/// assert!(!expression_contains_function_call("arr[0]"));
/// assert!(!expression_contains_function_call("(x + y)"));
/// ```
pub fn expression_contains_function_call(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'(' && i > 0 {
            let prev = bytes[i - 1];
            // Check if preceded by identifier char (alphanumeric, underscore, or dot for method calls)
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.' {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_functions() {
        assert!(expression_contains_function_call("len(x)"));
        assert!(expression_contains_function_call("cap(slice)"));
        assert!(expression_contains_function_call("make([]int, 10)"));
        assert!(expression_contains_function_call("append(slice, x)"));
    }

    #[test]
    fn test_package_functions() {
        assert!(expression_contains_function_call("fmt.Sprintf(\"%d\", x)"));
        assert!(expression_contains_function_call(
            "strings.Contains(s, \"foo\")"
        ));
        assert!(expression_contains_function_call("time.Now()"));
    }

    #[test]
    fn test_method_calls() {
        assert!(expression_contains_function_call("user.GetName()"));
        assert!(expression_contains_function_call("obj.Method()"));
        assert!(expression_contains_function_call("list.append(item)"));
    }

    #[test]
    fn test_nested() {
        assert!(expression_contains_function_call(
            "len(strings.Split(s, \",\"))"
        ));
        assert!(expression_contains_function_call(
            "fmt.Sprintf(\"%v\", obj.Get())"
        ));
    }

    #[test]
    fn test_simple_variables() {
        assert!(!expression_contains_function_call("x"));
        assert!(!expression_contains_function_call("user"));
        assert!(!expression_contains_function_call("myVar"));
    }

    #[test]
    fn test_field_access() {
        assert!(!expression_contains_function_call("user.name"));
        assert!(!expression_contains_function_call("obj.field"));
        assert!(!expression_contains_function_call("a.b.c"));
    }

    #[test]
    fn test_indexing() {
        assert!(!expression_contains_function_call("items[0]"));
        assert!(!expression_contains_function_call("map[key]"));
        assert!(!expression_contains_function_call("arr[i]"));
    }

    #[test]
    fn test_arithmetic() {
        assert!(!expression_contains_function_call("(x + y)"));
        assert!(!expression_contains_function_call("(a * b) + c"));
        assert!(!expression_contains_function_call("((x))"));
    }

    #[test]
    fn test_type_cast() {
        // Type casts like int(x) look like function calls and are detected as such.
        // This is acceptable - false positives are safer than false negatives for SafeMode.
        assert!(expression_contains_function_call("int(x)"));
        assert!(expression_contains_function_call("float64(value)"));
    }

    #[test]
    fn test_empty() {
        assert!(!expression_contains_function_call(""));
        assert!(!expression_contains_function_call("   "));
    }
}
