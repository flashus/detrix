//! Python-specific safety configuration and function classification constants

use super::LspConfig;
use crate::constants::{DEFAULT_ANALYSIS_TIMEOUT_MS, DEFAULT_MAX_CALL_DEPTH};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============================================================================
// Python Safety Config
// ============================================================================

/// Python-specific safety configuration
///
/// Built-in function lists come from the `PYTHON_PURE_FUNCTIONS`, `PYTHON_IMPURE_FUNCTIONS`,
/// and `PYTHON_ACCEPTABLE_IMPURE` constants. Users can extend these with additional functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonSafetyConfig {
    /// User's additional allowed functions (extend built-in PURE + ACCEPTABLE_IMPURE)
    #[serde(default)]
    pub user_allowed_functions: Vec<String>,

    /// User's additional prohibited functions (extend built-in IMPURE)
    #[serde(default)]
    pub user_prohibited_functions: Vec<String>,

    /// Sensitive variable patterns (substring patterns to block)
    #[serde(default = "default_python_sensitive_patterns")]
    pub sensitive_patterns: Vec<String>,

    /// LSP-based purity analysis settings for Python
    #[serde(default = "default_python_lsp_config")]
    pub lsp: LspConfig,
}

impl Default for PythonSafetyConfig {
    fn default() -> Self {
        Self {
            user_allowed_functions: Vec::new(),
            user_prohibited_functions: Vec::new(),
            sensitive_patterns: default_python_sensitive_patterns(),
            lsp: default_python_lsp_config(),
        }
    }
}

impl PythonSafetyConfig {
    /// Get effective allowed functions: BUILTIN_PURE + BUILTIN_ACCEPTABLE_IMPURE + user extensions
    pub fn allowed_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = PYTHON_PURE_FUNCTIONS
            .iter()
            .chain(PYTHON_ACCEPTABLE_IMPURE.iter())
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_allowed_functions.iter().cloned());
        set
    }

    /// Get effective prohibited functions: BUILTIN_IMPURE + user extensions
    pub fn prohibited_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = PYTHON_IMPURE_FUNCTIONS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_prohibited_functions.iter().cloned());
        set
    }

    /// Get sensitive variable patterns
    pub fn sensitive_patterns(&self) -> &Vec<String> {
        &self.sensitive_patterns
    }
}

fn default_python_lsp_config() -> LspConfig {
    LspConfig {
        enabled: false,
        command: "pyright-langserver".to_string(),
        args: vec!["--stdio".to_string()],
        max_call_depth: DEFAULT_MAX_CALL_DEPTH,
        analysis_timeout_ms: DEFAULT_ANALYSIS_TIMEOUT_MS,
        cache_enabled: true,
        working_directory: None,
    }
}

fn default_python_sensitive_patterns() -> Vec<String> {
    vec![
        // Credentials
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "auth_token",
        "access_token",
        "refresh_token",
        "bearer_token",
        "jwt",
        "token",
        // Database
        "db_password",
        "database_password",
        "connection_string",
        // Security
        "private_key",
        "ssh_key",
        "encryption_key",
        "signing_key",
        "credential",
        "credentials",
        // Personal data (GDPR/PII)
        "ssn",
        "social_security",
        "credit_card",
        "card_number",
        "cvv",
        "pin",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ============================================================================
// Python Function Classification Constants
// ============================================================================

/// Python "Acceptable" impure functions - I/O but safe for metrics
pub const PYTHON_ACCEPTABLE_IMPURE: &[&str] = &[
    "print",
    "logging.info",
    "logging.debug",
    "logging.warning",
    "logging.error",
    "logging.critical",
    "logging.log",
    "logger.info",
    "logger.debug",
    "logger.warning",
    "logger.error",
    "logger.critical",
];

/// Python pure functions (no side effects)
pub const PYTHON_PURE_FUNCTIONS: &[&str] = &[
    // Math module
    "math.sin",
    "math.cos",
    "math.tan",
    "math.sqrt",
    "math.pow",
    "math.log",
    "math.log10",
    "math.log2",
    "math.exp",
    "math.floor",
    "math.ceil",
    "math.trunc",
    "math.fabs",
    "math.factorial",
    "math.gcd",
    "math.lcm",
    "math.isnan",
    "math.isinf",
    "math.isfinite",
    // JSON
    "json.loads",
    "json.dumps",
    // Dict methods (non-mutating)
    "dict.keys",
    "dict.values",
    "dict.items",
    "dict.get",
    "dict.copy",
    "dict.fromkeys",
    "keys",
    "values",
    "items",
    "get",
    // List methods (non-mutating)
    "list.copy",
    "list.count",
    "list.index",
    "count",
    "index",
    "copy",
    // Set methods (non-mutating)
    "set.copy",
    "set.difference",
    "set.intersection",
    "set.isdisjoint",
    "set.issubset",
    "set.issuperset",
    "set.symmetric_difference",
    "set.union",
    "difference",
    "intersection",
    "isdisjoint",
    "issubset",
    "issuperset",
    "symmetric_difference",
    "union",
    // Built-in functions
    "len",
    "range",
    "enumerate",
    "zip",
    "map",
    "filter",
    "sorted",
    "reversed",
    "min",
    "max",
    "sum",
    "abs",
    "round",
    "isinstance",
    "issubclass",
    "hasattr",
    "getattr",
    "type",
    "id",
    "hash",
    "repr",
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "frozenset",
    "bytes",
    "bytearray",
    "ord",
    "chr",
    "bin",
    "oct",
    "hex",
    "format",
    "ascii",
    "callable",
    "vars",
    "dir",
    "all",
    "any",
    "iter",
    "next",
    "slice",
    // String methods
    "str.format",
    "str.join",
    "str.split",
    "str.rsplit",
    "str.strip",
    "str.lstrip",
    "str.rstrip",
    "str.lower",
    "str.upper",
    "str.title",
    "str.capitalize",
    "str.swapcase",
    "str.replace",
    "str.find",
    "str.rfind",
    "str.index",
    "str.rindex",
    "str.count",
    "str.startswith",
    "str.endswith",
    "str.isalpha",
    "str.isdigit",
    "str.isalnum",
    "str.isspace",
    "str.islower",
    "str.isupper",
    "str.istitle",
    "str.encode",
    "str.zfill",
    "str.center",
    "str.ljust",
    "str.rjust",
    "str.partition",
    "str.rpartition",
    "str.splitlines",
    "str.expandtabs",
    "str.translate",
    "str.maketrans",
    // Itertools
    "itertools.chain",
    "itertools.combinations",
    "itertools.combinations_with_replacement",
    "itertools.compress",
    "itertools.count",
    "itertools.cycle",
    "itertools.dropwhile",
    "itertools.filterfalse",
    "itertools.groupby",
    "itertools.islice",
    "itertools.permutations",
    "itertools.product",
    "itertools.repeat",
    "itertools.starmap",
    "itertools.takewhile",
    "itertools.tee",
    "itertools.zip_longest",
    // Functools
    "functools.reduce",
    "functools.partial",
    "functools.partialmethod",
    "functools.wraps",
    // Operator module
    "operator.add",
    "operator.sub",
    "operator.mul",
    "operator.truediv",
    "operator.floordiv",
    "operator.mod",
    "operator.pow",
    "operator.neg",
    "operator.pos",
    "operator.abs",
    "operator.eq",
    "operator.ne",
    "operator.lt",
    "operator.le",
    "operator.gt",
    "operator.ge",
    "operator.and_",
    "operator.or_",
    "operator.xor",
    "operator.not_",
    "operator.itemgetter",
    "operator.attrgetter",
    "operator.methodcaller",
    // Exception constructors
    "Exception",
    "ValueError",
    "TypeError",
    "KeyError",
    "IndexError",
    "AttributeError",
    "RuntimeError",
    "StopIteration",
    "AssertionError",
    "ImportError",
    "ModuleNotFoundError",
    "NameError",
    "ZeroDivisionError",
    "OverflowError",
    "MemoryError",
    "RecursionError",
    "NotImplementedError",
    "LookupError",
    "ArithmeticError",
    // Re module
    "re.match",
    "re.search",
    "re.findall",
    "re.finditer",
    "re.sub",
    "re.subn",
    "re.split",
    "re.compile",
    "re.escape",
    "re.fullmatch",
    // Copy module
    "copy.copy",
    "copy.deepcopy",
    // Datetime
    "datetime.datetime",
    "datetime.date",
    "datetime.time",
    "datetime.timedelta",
    "datetime.datetime.now",
    "datetime.datetime.utcnow",
    "datetime.datetime.fromisoformat",
    "datetime.datetime.strptime",
    // Collections
    "collections.Counter",
    "collections.OrderedDict",
    "collections.defaultdict",
    "collections.namedtuple",
    "collections.deque",
    // Math (Python 3.8+ additions)
    "math.dist",
    "math.prod",
    "math.isqrt",
    "math.comb",
    "math.perm",
    // String methods (Python 3.9+)
    "str.removeprefix",
    "str.removesuffix",
    "removeprefix",
    "removesuffix",
    // Functools (Python 3.9+)
    "functools.cache",
    "functools.cached_property",
    // Dataclasses
    "dataclasses.asdict",
    "dataclasses.astuple",
    "dataclasses.field",
    "dataclasses.fields",
    "dataclasses.is_dataclass",
    "dataclasses.replace",
    // Enum
    "enum.auto",
    "enum.Enum",
    "enum.IntEnum",
    "enum.Flag",
    "enum.IntFlag",
    // Typing
    "typing.get_type_hints",
    "typing.get_origin",
    "typing.get_args",
    "typing.cast",
    // Decimal (pure math)
    "decimal.Decimal",
    "decimal.getcontext",
    // Fractions
    "fractions.Fraction",
    // Statistics
    "statistics.mean",
    "statistics.median",
    "statistics.mode",
    "statistics.stdev",
    "statistics.variance",
    "statistics.pstdev",
    "statistics.pvariance",
    "statistics.harmonic_mean",
    "statistics.geometric_mean",
    "statistics.quantiles",
    "statistics.fmean",
    // Pathlib (read-only operations)
    "pathlib.Path",
    "pathlib.PurePath",
    "pathlib.Path.exists",
    "pathlib.Path.is_file",
    "pathlib.Path.is_dir",
    "pathlib.Path.is_symlink",
    "pathlib.Path.is_absolute",
    "pathlib.Path.is_relative_to",
    "pathlib.Path.match",
    "pathlib.Path.name",
    "pathlib.Path.stem",
    "pathlib.Path.suffix",
    "pathlib.Path.parent",
    "pathlib.Path.parts",
    "pathlib.Path.as_posix",
    "pathlib.Path.with_name",
    "pathlib.Path.with_stem",
    "pathlib.Path.with_suffix",
    "pathlib.Path.joinpath",
    // Contextlib (context creation is pure)
    "contextlib.suppress",
    "contextlib.nullcontext",
    "contextlib.ExitStack",
    // UUID (pure generation)
    "uuid.uuid1",
    "uuid.uuid3",
    "uuid.uuid4",
    "uuid.uuid5",
    "uuid.UUID",
    // Base64
    "base64.b64encode",
    "base64.b64decode",
    "base64.urlsafe_b64encode",
    "base64.urlsafe_b64decode",
    // Hashlib (pure hashing)
    "hashlib.md5",
    "hashlib.sha1",
    "hashlib.sha256",
    "hashlib.sha512",
    "hashlib.blake2b",
    "hashlib.blake2s",
    // Struct
    "struct.pack",
    "struct.unpack",
    "struct.calcsize",
    // TOML (Python 3.11+)
    "tomllib.loads",
    // Zlib
    "zlib.compress",
    "zlib.decompress",
    "zlib.crc32",
    "zlib.adler32",
];

/// Python truly impure functions (always impure)
pub const PYTHON_IMPURE_FUNCTIONS: &[&str] = &[
    // I/O operations
    "open",
    "input",
    // File operations
    "os.remove",
    "os.unlink",
    "os.rename",
    "os.mkdir",
    "os.makedirs",
    "os.rmdir",
    "os.removedirs",
    "os.chdir",
    "os.chmod",
    "os.chown",
    "os.link",
    "os.symlink",
    "os.system",
    "os.popen",
    "os.exec",
    "os.execl",
    "os.execle",
    "os.execlp",
    "os.execlpe",
    "os.execv",
    "os.execve",
    "os.execvp",
    "os.execvpe",
    "os.fork",
    "os.kill",
    "os.killpg",
    "shutil.rmtree",
    "shutil.copy",
    "shutil.copy2",
    "shutil.move",
    // Subprocess
    "subprocess.run",
    "subprocess.call",
    "subprocess.check_call",
    "subprocess.check_output",
    "subprocess.Popen",
    // Network
    "socket.socket",
    "socket.connect",
    "urllib.request.urlopen",
    "requests.get",
    "requests.post",
    "requests.put",
    "requests.delete",
    "requests.patch",
    "httpx.get",
    "httpx.post",
    // Database
    "sqlite3.connect",
    "psycopg2.connect",
    "pymysql.connect",
    // Global state
    "globals",
    "setattr",
    "delattr",
    // Random state
    "random.seed",
    "random.setstate",
    // Threading
    "threading.Thread",
    "multiprocessing.Process",
    // Dangerous
    "eval",
    "exec",
    "compile",
    "__import__",
    "breakpoint",
    // System
    "sys.exit",
    "exit",
    "quit",
    // Time
    "time.sleep",
    // Pickle (I/O)
    "pickle.load",
    "pickle.dump",
    "pickle.loads",
    "pickle.dumps",
    // Pathlib write operations
    "pathlib.Path.write_text",
    "pathlib.Path.write_bytes",
    "pathlib.Path.mkdir",
    "pathlib.Path.rmdir",
    "pathlib.Path.unlink",
    "pathlib.Path.rename",
    "pathlib.Path.touch",
    "pathlib.Path.read_text",
    "pathlib.Path.read_bytes",
    // Async operations (impure - manage event loop state)
    "asyncio.run",
    "asyncio.create_task",
    "asyncio.gather",
    "asyncio.wait",
    "asyncio.wait_for",
    "asyncio.sleep",
    "asyncio.open_connection",
    "asyncio.start_server",
    "asyncio.get_event_loop",
    "asyncio.new_event_loop",
    "asyncio.set_event_loop",
    // Async HTTP clients
    "aiohttp.ClientSession",
    "aiohttp.get",
    "aiohttp.post",
    "httpx.AsyncClient",
    // TOML file I/O (Python 3.11+)
    "tomllib.load",
    // Tempfile operations
    "tempfile.mktemp",
    "tempfile.mkstemp",
    "tempfile.mkdtemp",
    "tempfile.NamedTemporaryFile",
    "tempfile.SpooledTemporaryFile",
    "tempfile.TemporaryDirectory",
    "tempfile.TemporaryFile",
    // Signal handling
    "signal.signal",
    "signal.alarm",
    "signal.pause",
    // Atexit
    "atexit.register",
    "atexit.unregister",
];

/// Python mutation methods (scope-aware)
pub const PYTHON_MUTATION_METHODS: &[&str] = &[
    // List
    "append",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "sort",
    "reverse",
    // Dict
    "update",
    "popitem",
    "setdefault",
    // Set
    "add",
    "discard",
    "intersection_update",
    "difference_update",
    "symmetric_difference_update",
    // Deque
    "appendleft",
    "extendleft",
    "popleft",
    "rotate",
];
