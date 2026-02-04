"""Tests for state management."""


from detrix._state import ClientState, State, get_state, reset_state


class TestState:
    """Test State enum."""

    def test_state_values(self):
        """Test state enum values."""
        assert State.SLEEPING.value == "sleeping"
        assert State.WAKING.value == "waking"
        assert State.AWAKE.value == "awake"


class TestClientState:
    """Test ClientState dataclass."""

    def test_default_values(self):
        """Test default state values."""
        state = ClientState()
        assert state.state == State.SLEEPING
        assert state.name == ""
        assert state.control_host == "127.0.0.1"
        assert state.control_port == 0
        assert state.debug_port == 0
        assert state.daemon_url == "http://127.0.0.1:8090"
        assert state.connection_id is None
        assert state.actual_control_port == 0
        assert state.actual_debug_port == 0
        assert state.detrix_home is None
        # Timeout defaults
        assert state.health_check_timeout == 2.0
        assert state.register_timeout == 5.0
        assert state.unregister_timeout == 2.0

    def test_lock_is_reentrant(self):
        """Test that lock is reentrant."""
        state = ClientState()
        with state.lock, state.lock:  # Should not deadlock
            state.state = State.AWAKE
        assert state.state == State.AWAKE

    def test_wake_lock_exists(self):
        """Test that wake_lock is a separate Lock."""
        state = ClientState()
        # wake_lock should be a regular Lock, not RLock
        assert hasattr(state, "wake_lock")
        # Should be acquirable
        acquired = state.wake_lock.acquire(blocking=False)
        assert acquired
        state.wake_lock.release()

    def test_state_has_ssl_fields(self):
        """Test that ClientState has SSL configuration fields."""
        state = ClientState()
        assert state.verify_ssl is True
        assert state.ca_bundle is None


class TestGlobalState:
    """Test global state management."""

    def setup_method(self):
        """Reset state before each test."""
        reset_state()

    def teardown_method(self):
        """Reset state after each test."""
        reset_state()

    def test_get_state_creates_singleton(self):
        """Test that get_state creates a singleton."""
        state1 = get_state()
        state2 = get_state()
        assert state1 is state2

    def test_reset_state_clears_singleton(self):
        """Test that reset_state clears the singleton."""
        state1 = get_state()
        state1.name = "test"
        reset_state()
        state2 = get_state()
        assert state2.name == ""
        assert state1 is not state2
