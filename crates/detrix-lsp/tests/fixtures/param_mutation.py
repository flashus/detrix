"""
Test fixture: Parameter mutation detection with scope-aware analysis

Tests the integration of extract_parameters() with analyze_function_body_with_params().
Functions here demonstrate:
- Impure: functions that mutate parameters (items.append, etc.)
- Pure: functions that only read parameters (len(items), etc.)
"""


def mutates_param(items: list, value: int) -> None:
    """IMPURE: Mutates the 'items' parameter directly."""
    items.append(value)


def mutates_param_method(data: dict, key: str, value: str) -> None:
    """IMPURE: Mutates the 'data' parameter via update."""
    data.update({key: value})


def mutates_self_attr(self, value: int) -> None:
    """IMPURE: Mutates self attribute."""
    self.items.append(value)


def reads_param_only(items: list) -> int:
    """PURE: Only reads from 'items', no mutation."""
    return len(items)


def processes_param_pure(items: list, factor: int) -> list:
    """PURE: Creates new list, doesn't mutate parameter."""
    result = []
    for item in items:
        result.append(item * factor)
    return result


def local_mutation_only(count: int) -> list:
    """PURE: Only mutates local 'result', not parameter."""
    result = []
    result.append(count)
    result.append(count + 1)
    return result


def nested_local_mutation(items: list) -> list:
    """PURE: 'copy' is a local variable, mutation is safe."""
    copy = list(items)  # Creates a new list
    copy.append(999)  # Mutates local copy, not param
    return copy


def mixed_mutation(items: list, output: list) -> None:
    """IMPURE: Mutates 'output' parameter, but reads 'items' only."""
    for item in items:
        output.append(item)


def global_mutation(items: list) -> int:
    """IMPURE: Uses global statement to modify global state."""
    global _counter
    _counter += 1
    return len(items)


# Global for testing global mutation detection
_counter = 0
