"""
Test fixture: Transitive impure function call chain

Call chain: entry_point -> middle_layer -> read_file
The impure open() call is 2 levels deep.
"""


def read_file(path: str) -> str:
    """Impure function - reads from file system."""
    with open(path, "r") as f:
        return f.read()


def middle_layer(path: str) -> str:
    """Middle function - calls impure read_file."""
    content = read_file(path)
    return content.upper()


def entry_point(path: str) -> str:
    """Entry point - transitively calls impure read_file."""
    return middle_layer(path)
