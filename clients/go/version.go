package detrix

// Version information (can be set via ldflags at build time)
var (
	// Version is the client version (set via ldflags or defaults to "1.0.0")
	Version = "1.0.0"

	// BuildCommit is the git commit SHA (set via ldflags)
	BuildCommit = ""

	// BuildTime is when the binary was built (set via ldflags)
	BuildTime = ""
)
