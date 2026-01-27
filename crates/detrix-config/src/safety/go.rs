//! Go-specific safety configuration and function classification constants

use super::LspConfig;
use crate::constants::{DEFAULT_ANALYSIS_TIMEOUT_MS, DEFAULT_MAX_CALL_DEPTH};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============================================================================
// Go Safety Config
// ============================================================================

/// Go-specific safety configuration
///
/// Built-in function lists come from the `GO_PURE_FUNCTIONS`, `GO_IMPURE_FUNCTIONS`,
/// and `GO_ACCEPTABLE_IMPURE` constants. Users can extend these with additional functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoSafetyConfig {
    /// User's additional allowed functions (extend built-in PURE + ACCEPTABLE_IMPURE)
    #[serde(default)]
    pub user_allowed_functions: Vec<String>,

    /// User's additional prohibited functions (extend built-in IMPURE)
    #[serde(default)]
    pub user_prohibited_functions: Vec<String>,

    /// Sensitive variable patterns (substring patterns to block)
    #[serde(default = "default_go_sensitive_patterns")]
    pub sensitive_patterns: Vec<String>,

    /// LSP-based purity analysis settings for Go
    #[serde(default = "default_go_lsp_config")]
    pub lsp: LspConfig,
}

impl Default for GoSafetyConfig {
    fn default() -> Self {
        Self {
            user_allowed_functions: Vec::new(),
            user_prohibited_functions: Vec::new(),
            sensitive_patterns: default_go_sensitive_patterns(),
            lsp: default_go_lsp_config(),
        }
    }
}

impl GoSafetyConfig {
    /// Get effective allowed functions: BUILTIN_PURE + BUILTIN_ACCEPTABLE_IMPURE + user extensions
    pub fn allowed_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = GO_PURE_FUNCTIONS
            .iter()
            .chain(GO_ACCEPTABLE_IMPURE.iter())
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_allowed_functions.iter().cloned());
        set
    }

    /// Get effective prohibited functions: BUILTIN_IMPURE + user extensions
    pub fn prohibited_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = GO_IMPURE_FUNCTIONS
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

fn default_go_lsp_config() -> LspConfig {
    LspConfig {
        enabled: false,
        command: "gopls".to_string(),
        args: vec!["serve".to_string()],
        max_call_depth: DEFAULT_MAX_CALL_DEPTH,
        analysis_timeout_ms: DEFAULT_ANALYSIS_TIMEOUT_MS,
        cache_enabled: true,
        working_directory: None,
    }
}

fn default_go_sensitive_patterns() -> Vec<String> {
    vec![
        // Credentials
        "password",
        "passwd",
        "secret",
        "apiKey",
        "api_key",
        "authToken",
        "auth_token",
        "accessToken",
        "access_token",
        "refreshToken",
        "refresh_token",
        "bearerToken",
        "bearer_token",
        "jwt",
        "token",
        // Database
        "dbPassword",
        "db_password",
        "connectionString",
        "connection_string",
        // Security
        "privateKey",
        "private_key",
        "sshKey",
        "ssh_key",
        "encryptionKey",
        "encryption_key",
        "signingKey",
        "signing_key",
        "credential",
        "credentials",
        // Personal data (GDPR/PII)
        "ssn",
        "socialSecurity",
        "creditCard",
        "cardNumber",
        "cvv",
        "pin",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ============================================================================
// Go Function Classification Constants
// ============================================================================

/// Go "Acceptable" impure functions - I/O but safe for metrics (logging/printing)
pub const GO_ACCEPTABLE_IMPURE: &[&str] = &[
    "fmt.Print",
    "fmt.Printf",
    "fmt.Println",
    "fmt.Fprint",
    "fmt.Fprintf",
    "fmt.Fprintln",
    "log.Print",
    "log.Printf",
    "log.Println",
    // slog (Go 1.21+ structured logging)
    "slog.Debug",
    "slog.DebugContext",
    "slog.Info",
    "slog.InfoContext",
    "slog.Warn",
    "slog.WarnContext",
    "slog.Error",
    "slog.ErrorContext",
    "slog.Log",
    "slog.LogAttrs",
];

/// Go pure functions (no side effects)
pub const GO_PURE_FUNCTIONS: &[&str] = &[
    // Built-in functions
    "len",
    "cap",
    "make",
    "new",
    "append", // Returns new slice, original unchanged if cap exceeded
    "max",
    "min",
    "complex",
    "imag",
    "real",
    "panic",   // Control flow, doesn't modify state
    "recover", // Control flow, doesn't modify state
    // Math package
    "math.Abs",
    "math.Acos",
    "math.Acosh",
    "math.Asin",
    "math.Asinh",
    "math.Atan",
    "math.Atan2",
    "math.Atanh",
    "math.Cbrt",
    "math.Ceil",
    "math.Copysign",
    "math.Cos",
    "math.Cosh",
    "math.Dim",
    "math.Erf",
    "math.Erfc",
    "math.Erfcinv",
    "math.Erfinv",
    "math.Exp",
    "math.Exp2",
    "math.Expm1",
    "math.FMA",
    "math.Float32bits",
    "math.Float32frombits",
    "math.Float64bits",
    "math.Float64frombits",
    "math.Floor",
    "math.Frexp",
    "math.Gamma",
    "math.Hypot",
    "math.Ilogb",
    "math.Inf",
    "math.IsInf",
    "math.IsNaN",
    "math.J0",
    "math.J1",
    "math.Jn",
    "math.Ldexp",
    "math.Lgamma",
    "math.Log",
    "math.Log10",
    "math.Log1p",
    "math.Log2",
    "math.Logb",
    "math.Max",
    "math.Min",
    "math.Mod",
    "math.Modf",
    "math.NaN",
    "math.Nextafter",
    "math.Nextafter32",
    "math.Pow",
    "math.Pow10",
    "math.Remainder",
    "math.Round",
    "math.RoundToEven",
    "math.Signbit",
    "math.Sin",
    "math.Sincos",
    "math.Sinh",
    "math.Sqrt",
    "math.Tan",
    "math.Tanh",
    "math.Trunc",
    "math.Y0",
    "math.Y1",
    "math.Yn",
    // Strings package
    "strings.Clone",
    "strings.Compare",
    "strings.Contains",
    "strings.ContainsAny",
    "strings.ContainsRune",
    "strings.Count",
    "strings.Cut",
    "strings.CutPrefix",
    "strings.CutSuffix",
    "strings.EqualFold",
    "strings.Fields",
    "strings.FieldsFunc",
    "strings.HasPrefix",
    "strings.HasSuffix",
    "strings.Index",
    "strings.IndexAny",
    "strings.IndexByte",
    "strings.IndexFunc",
    "strings.IndexRune",
    "strings.Join",
    "strings.LastIndex",
    "strings.LastIndexAny",
    "strings.LastIndexByte",
    "strings.LastIndexFunc",
    "strings.Map",
    "strings.Repeat",
    "strings.Replace",
    "strings.ReplaceAll",
    "strings.Split",
    "strings.SplitAfter",
    "strings.SplitAfterN",
    "strings.SplitN",
    "strings.Title",
    "strings.ToLower",
    "strings.ToLowerSpecial",
    "strings.ToTitle",
    "strings.ToTitleSpecial",
    "strings.ToUpper",
    "strings.ToUpperSpecial",
    "strings.ToValidUTF8",
    "strings.Trim",
    "strings.TrimFunc",
    "strings.TrimLeft",
    "strings.TrimLeftFunc",
    "strings.TrimPrefix",
    "strings.TrimRight",
    "strings.TrimRightFunc",
    "strings.TrimSpace",
    "strings.TrimSuffix",
    // Strconv package
    "strconv.AppendBool",
    "strconv.AppendFloat",
    "strconv.AppendInt",
    "strconv.AppendQuote",
    "strconv.AppendQuoteRune",
    "strconv.AppendQuoteRuneToASCII",
    "strconv.AppendQuoteRuneToGraphic",
    "strconv.AppendQuoteToASCII",
    "strconv.AppendQuoteToGraphic",
    "strconv.AppendUint",
    "strconv.Atoi",
    "strconv.CanBackquote",
    "strconv.FormatBool",
    "strconv.FormatComplex",
    "strconv.FormatFloat",
    "strconv.FormatInt",
    "strconv.FormatUint",
    "strconv.IsGraphic",
    "strconv.IsPrint",
    "strconv.Itoa",
    "strconv.ParseBool",
    "strconv.ParseComplex",
    "strconv.ParseFloat",
    "strconv.ParseInt",
    "strconv.ParseUint",
    "strconv.Quote",
    "strconv.QuoteRune",
    "strconv.QuoteRuneToASCII",
    "strconv.QuoteRuneToGraphic",
    "strconv.QuoteToASCII",
    "strconv.QuoteToGraphic",
    "strconv.Unquote",
    "strconv.UnquoteChar",
    // JSON
    "json.Marshal",
    "json.MarshalIndent",
    "json.Unmarshal",
    "json.Valid",
    // Time package
    "time.Now",
    "time.Since",
    "time.Until",
    "time.Parse",
    "time.ParseDuration",
    "time.ParseInLocation",
    "time.Date",
    "time.Unix",
    "time.UnixMicro",
    "time.UnixMilli",
    // Sort package
    "sort.Ints",
    "sort.Float64s",
    "sort.Strings",
    "sort.IntsAreSorted",
    "sort.Float64sAreSorted",
    "sort.StringsAreSorted",
    "sort.IsSorted",
    "sort.Search",
    "sort.SearchFloat64s",
    "sort.SearchInts",
    "sort.SearchStrings",
    // Reflect package
    "reflect.TypeOf",
    "reflect.ValueOf",
    "reflect.DeepEqual",
    // fmt.Sprint* (return string)
    "fmt.Sprint",
    "fmt.Sprintf",
    "fmt.Sprintln",
    "fmt.Errorf",
    // Errors package
    "errors.New",
    "errors.Is",
    "errors.As",
    "errors.Unwrap",
    // Bytes package
    "bytes.Clone",
    "bytes.Compare",
    "bytes.Contains",
    "bytes.ContainsAny",
    "bytes.ContainsRune",
    "bytes.Count",
    "bytes.Cut",
    "bytes.CutPrefix",
    "bytes.CutSuffix",
    "bytes.Equal",
    "bytes.EqualFold",
    "bytes.Fields",
    "bytes.FieldsFunc",
    "bytes.HasPrefix",
    "bytes.HasSuffix",
    "bytes.Index",
    "bytes.IndexAny",
    "bytes.IndexByte",
    "bytes.IndexFunc",
    "bytes.IndexRune",
    "bytes.Join",
    "bytes.LastIndex",
    "bytes.LastIndexAny",
    "bytes.LastIndexByte",
    "bytes.LastIndexFunc",
    "bytes.Map",
    "bytes.Repeat",
    "bytes.Replace",
    "bytes.ReplaceAll",
    "bytes.Runes",
    "bytes.Split",
    "bytes.SplitAfter",
    "bytes.SplitAfterN",
    "bytes.SplitN",
    "bytes.Title",
    "bytes.ToLower",
    "bytes.ToLowerSpecial",
    "bytes.ToTitle",
    "bytes.ToTitleSpecial",
    "bytes.ToUpper",
    "bytes.ToUpperSpecial",
    "bytes.ToValidUTF8",
    "bytes.Trim",
    "bytes.TrimFunc",
    "bytes.TrimLeft",
    "bytes.TrimLeftFunc",
    "bytes.TrimPrefix",
    "bytes.TrimRight",
    "bytes.TrimRightFunc",
    "bytes.TrimSpace",
    "bytes.TrimSuffix",
    // Unicode package
    "unicode.In",
    "unicode.Is",
    "unicode.IsControl",
    "unicode.IsDigit",
    "unicode.IsGraphic",
    "unicode.IsLetter",
    "unicode.IsLower",
    "unicode.IsMark",
    "unicode.IsNumber",
    "unicode.IsOneOf",
    "unicode.IsPrint",
    "unicode.IsPunct",
    "unicode.IsSpace",
    "unicode.IsSymbol",
    "unicode.IsTitle",
    "unicode.IsUpper",
    "unicode.SimpleFold",
    "unicode.To",
    "unicode.ToLower",
    "unicode.ToTitle",
    "unicode.ToUpper",
    // Slices package (Go 1.21+)
    "slices.BinarySearch",
    "slices.BinarySearchFunc",
    "slices.Clip",
    "slices.Clone",
    "slices.Compact",
    "slices.CompactFunc",
    "slices.Compare",
    "slices.CompareFunc",
    "slices.Concat",
    "slices.Contains",
    "slices.ContainsFunc",
    "slices.Delete",
    "slices.DeleteFunc",
    "slices.Equal",
    "slices.EqualFunc",
    "slices.Grow",
    "slices.Index",
    "slices.IndexFunc",
    "slices.Insert",
    "slices.IsSorted",
    "slices.IsSortedFunc",
    "slices.Max",
    "slices.MaxFunc",
    "slices.Min",
    "slices.MinFunc",
    "slices.Replace",
    "slices.Reverse",
    "slices.Sort",
    "slices.SortFunc",
    "slices.SortStableFunc",
    // Maps package (Go 1.21+)
    "maps.Clone",
    "maps.Copy",
    "maps.DeleteFunc",
    "maps.Equal",
    "maps.EqualFunc",
    "maps.Keys",
    "maps.Values",
    // Cmp package (Go 1.21+)
    "cmp.Compare",
    "cmp.Less",
    "cmp.Or",
    // Regexp package (pure pattern matching)
    "regexp.Compile",
    "regexp.CompilePOSIX",
    "regexp.Match",
    "regexp.MatchReader",
    "regexp.MatchString",
    "regexp.MustCompile",
    "regexp.MustCompilePOSIX",
    "regexp.QuoteMeta",
    // Encoding packages (pure transforms)
    "encoding/json.Marshal",
    "encoding/json.MarshalIndent",
    "encoding/json.Unmarshal",
    "encoding/json.Valid",
    "encoding/base64.StdEncoding.EncodeToString",
    "encoding/base64.StdEncoding.DecodeString",
    "encoding/hex.EncodeToString",
    "encoding/hex.DecodeString",
    "encoding/binary.BigEndian.Uint16",
    "encoding/binary.BigEndian.Uint32",
    "encoding/binary.BigEndian.Uint64",
    "encoding/binary.LittleEndian.Uint16",
    "encoding/binary.LittleEndian.Uint32",
    "encoding/binary.LittleEndian.Uint64",
    // Path/filepath package (pure path manipulation)
    "path.Base",
    "path.Clean",
    "path.Dir",
    "path.Ext",
    "path.IsAbs",
    "path.Join",
    "path.Match",
    "path.Split",
    "filepath.Abs",
    "filepath.Base",
    "filepath.Clean",
    "filepath.Dir",
    "filepath.Ext",
    "filepath.FromSlash",
    "filepath.IsAbs",
    "filepath.IsLocal",
    "filepath.Join",
    "filepath.Match",
    "filepath.Rel",
    "filepath.Split",
    "filepath.SplitList",
    "filepath.ToSlash",
    "filepath.VolumeName",
    // Crypto hash (pure transforms)
    "crypto/md5.Sum",
    "crypto/sha1.Sum",
    "crypto/sha256.Sum256",
    "crypto/sha512.Sum512",
    // slog helpers (Go 1.21+) - pure construction
    "slog.With",
    "slog.Group",
    "slog.String",
    "slog.Int",
    "slog.Int64",
    "slog.Uint64",
    "slog.Float64",
    "slog.Bool",
    "slog.Time",
    "slog.Duration",
    "slog.Any",
    // Sync/atomic (pure read operations)
    "atomic.LoadInt32",
    "atomic.LoadInt64",
    "atomic.LoadUint32",
    "atomic.LoadUint64",
    "atomic.LoadPointer",
    "atomic.LoadUintptr",
];

/// Go impure functions (side effects)
pub const GO_IMPURE_FUNCTIONS: &[&str] = &[
    // Log functions that exit/panic
    "log.Fatal",
    "log.Fatalf",
    "log.Fatalln",
    "log.Panic",
    "log.Panicf",
    "log.Panicln",
    // OS operations
    "os.Chdir",
    "os.Chmod",
    "os.Chown",
    "os.Chtimes",
    "os.Create",
    "os.CreateTemp",
    "os.Exit",
    "os.Mkdir",
    "os.MkdirAll",
    "os.MkdirTemp",
    "os.Open",
    "os.OpenFile",
    "os.ReadFile",
    "os.Remove",
    "os.RemoveAll",
    "os.Rename",
    "os.Setenv",
    "os.Truncate",
    "os.Unsetenv",
    "os.WriteFile",
    // exec package
    "exec.Command",
    "exec.CommandContext",
    "exec.LookPath",
    // io operations
    "io.Copy",
    "io.CopyBuffer",
    "io.CopyN",
    "io.Pipe",
    "io.ReadAll",
    "io.ReadAtLeast",
    "io.ReadFull",
    "io.WriteString",
    // ioutil
    "ioutil.ReadAll",
    "ioutil.ReadDir",
    "ioutil.ReadFile",
    "ioutil.TempDir",
    "ioutil.TempFile",
    "ioutil.WriteFile",
    // Net operations
    "net.Dial",
    "net.DialIP",
    "net.DialTCP",
    "net.DialTimeout",
    "net.DialUDP",
    "net.DialUnix",
    "net.Listen",
    "net.ListenIP",
    "net.ListenMulticastUDP",
    "net.ListenPacket",
    "net.ListenTCP",
    "net.ListenUDP",
    "net.ListenUnix",
    "net.ListenUnixgram",
    // HTTP operations
    "http.Get",
    "http.Head",
    "http.NewRequest",
    "http.NewRequestWithContext",
    "http.Post",
    "http.PostForm",
    // Database
    "sql.Open",
    "sql.OpenDB",
    // Syscall
    "syscall.Chdir",
    "syscall.Chmod",
    "syscall.Chown",
    "syscall.Exec",
    "syscall.Exit",
    "syscall.Fork",
    "syscall.ForkExec",
    "syscall.Kill",
    "syscall.Mkdir",
    "syscall.Mknod",
    "syscall.Mount",
    "syscall.Open",
    "syscall.Rename",
    "syscall.Rmdir",
    "syscall.Setenv",
    "syscall.Symlink",
    "syscall.Unlink",
    "syscall.Unmount",
    "syscall.Write",
    // Runtime
    "runtime.GC",
    "runtime.GOMAXPROCS",
    "runtime.Goexit",
    "runtime.SetFinalizer",
    // Unsafe
    "unsafe.Add",
    "unsafe.Alignof",
    "unsafe.Offsetof",
    "unsafe.Pointer",
    "unsafe.Sizeof",
    "unsafe.Slice",
    "unsafe.SliceData",
    "unsafe.String",
    "unsafe.StringData",
    // Plugin
    "plugin.Open",
    // CGO
    "C.malloc",
    "C.free",
    "C.CString",
    "C.GoString",
    "C.GoBytes",
    // Context (creates resources)
    "context.WithCancel",
    "context.WithDeadline",
    "context.WithTimeout",
    "context.WithValue",
    "context.Background",
    "context.TODO",
    // Sync/atomic write operations
    "atomic.StoreInt32",
    "atomic.StoreInt64",
    "atomic.StoreUint32",
    "atomic.StoreUint64",
    "atomic.StorePointer",
    "atomic.StoreUintptr",
    "atomic.AddInt32",
    "atomic.AddInt64",
    "atomic.AddUint32",
    "atomic.AddUint64",
    "atomic.AddUintptr",
    "atomic.SwapInt32",
    "atomic.SwapInt64",
    "atomic.SwapUint32",
    "atomic.SwapUint64",
    "atomic.SwapPointer",
    "atomic.SwapUintptr",
    "atomic.CompareAndSwapInt32",
    "atomic.CompareAndSwapInt64",
    "atomic.CompareAndSwapUint32",
    "atomic.CompareAndSwapUint64",
    "atomic.CompareAndSwapPointer",
    "atomic.CompareAndSwapUintptr",
    // Filepath walk (I/O)
    "filepath.Walk",
    "filepath.WalkDir",
    "filepath.Glob",
    // Time operations with side effects
    "time.AfterFunc",
    "time.NewTimer",
    "time.NewTicker",
    "time.Sleep",
    "time.After",
    "time.Tick",
    // Channel operations
    "close",
];

/// Go mutation operations (scope-aware)
/// These modify their arguments - impure when modifying non-local state
pub const GO_MUTATION_OPERATIONS: &[&str] = &[
    "close",  // Closes a channel
    "copy",   // copy(dst, src) - modifies dst slice
    "delete", // delete(m, key) - removes key from map
    "clear",  // clear(m) - removes all entries from map/slice
];

/// Go receiver mutation methods
pub const GO_RECEIVER_MUTATION_METHODS: &[&str] = &[
    // sync package
    "Lock",
    "Unlock",
    "RLock",
    "RUnlock",
    // sync.WaitGroup
    "Add",
    "Done",
    "Wait",
    // sync.Cond
    "Signal",
    "Broadcast",
    // sync.Once
    "Do",
    // sync.Pool
    "Get",
    "Put",
    // bytes.Buffer
    "Write",
    "WriteByte",
    "WriteRune",
    "WriteString",
    "Reset",
    "Truncate",
    "ReadFrom",
    // Container mutations
    "PushBack",
    "PushFront",
    "Remove",
    "InsertBefore",
    "InsertAfter",
    "MoveToFront",
    "MoveToBack",
    "MoveBefore",
    "MoveAfter",
    // bufio.Writer
    "Flush",
    // context
    "CancelFunc",
];
