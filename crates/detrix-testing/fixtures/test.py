# Test fixture file for Detrix tests
# This file provides sample Python code for metric testing
# Important lines are documented for test reference

"""
Detrix Test Fixture Module

This module provides mock classes and utility functions for testing
the Detrix observability platform. Each class and function at specific
lines is referenced by integration tests.

Line References:
- Line 30: x = Value() - used by sample_metric()
- Line 100: request = Request() - used by tests requiring Request
- Line 127: user = User() - used by tests requiring User
- Line 250: transaction = Transaction() - used by tests requiring Transaction
"""


class Value:
    """Generic value wrapper for testing expressions."""
    def __init__(self, value=None):
        self.value = value
        self.data = {}

    def __repr__(self):
        return f"Value({self.value})"

# Line 30 - Variables used by sample_metric()
x = Value(10)
y = Value(20)
z = Value(30)
value = Value(42)
test_var = Value("test")
updated_expression = Value("updated")

# Variables for concurrent tests (x0 through x19)
x0, x1, x2, x3, x4 = Value(0), Value(1), Value(2), Value(3), Value(4)
x5, x6, x7, x8, x9 = Value(5), Value(6), Value(7), Value(8), Value(9)
x10, x11, x12, x13, x14 = Value(10), Value(11), Value(12), Value(13), Value(14)
x15, x16, x17, x18, x19 = Value(15), Value(16), Value(17), Value(18), Value(19)


class Request:
    """Mock HTTP request object for testing."""
    def __init__(self, user_id=None, session_id=None):
        self.user_id = user_id
        self.session_id = session_id
        self.method = "GET"
        self.path = "/"
        self.headers = {}
        self.body = None

    def is_authenticated(self):
        return self.user_id is not None


class User:
    """Mock user object for authentication testing."""
    def __init__(self, user_id=None, name=None):
        self.id = user_id
        self.name = name
        self.session_id = None
        self.email = None
        self.roles = []

    def has_role(self, role):
        return role in self.roles


class Transaction:
    """Mock transaction object for payment testing."""
    def __init__(self, amount=0, currency="USD"):
        self.amount = amount
        self.currency = currency
        self.status = "pending"
        self.transaction_id = None

    def is_complete(self):
        return self.status == "complete"


def process_request(request):
    """Process an incoming request and return user context."""
    user = User(request.user_id)
    return user


def handle_transaction(transaction):
    """Process a transaction and return the amount."""
    if transaction.amount > 0:
        transaction.status = "processing"
    return transaction.amount


def calculate_total(items):
    """Calculate total price of items."""
    total = 0
    for item in items:
        total += getattr(item, 'price', 0)
    return total


def lifecycle_test():
    """Test function for lifecycle/integration tests."""
    return {"status": "ok", "message": "lifecycle test passed"}


# Line 100 - Request instance used by tests
request = Request(user_id=1, session_id="sess_001")


def validate_request(req):
    """Validate a request object."""
    if not req.user_id:
        raise ValueError("Missing user_id")
    return True


def authenticate(user_id, password):
    """Mock authentication function."""
    if user_id and password:
        return User(user_id, name="TestUser")
    return None


def get_session(session_id):
    """Retrieve session by ID."""
    return {"id": session_id, "user_id": 1, "created_at": "2024-01-01"}


# Line 127 - User instance used by tests
user = User(user_id=42, name="TestUser")
user.email = "test@example.com"
user.roles = ["user", "tester"]


def process_user_action(user, action):
    """Process an action for a user."""
    return {"user_id": user.id, "action": action, "success": True}


def get_user_permissions(user):
    """Get permissions for a user."""
    base_permissions = ["read"]
    if "admin" in user.roles:
        base_permissions.extend(["write", "delete"])
    return base_permissions


class OrderItem:
    """Mock order item for testing calculations."""
    def __init__(self, name, price, quantity=1):
        self.name = name
        self.price = price
        self.quantity = quantity

    def total(self):
        return self.price * self.quantity


class Order:
    """Mock order for testing complex expressions."""
    def __init__(self):
        self.items = []
        self.customer = None
        self.status = "draft"

    def add_item(self, item):
        self.items.append(item)

    def subtotal(self):
        return sum(item.total() for item in self.items)

    def calculate_tax(self, rate=0.1):
        return self.subtotal() * rate

    def grand_total(self):
        return self.subtotal() + self.calculate_tax()


def process_order(order):
    """Process an order and update status."""
    if order.items:
        order.status = "processing"
        return order.grand_total()
    return 0


def apply_discount(amount, discount_percent):
    """Apply a percentage discount to an amount."""
    if discount_percent < 0 or discount_percent > 100:
        raise ValueError("Invalid discount percentage")
    return amount * (1 - discount_percent / 100)


class PaymentProcessor:
    """Mock payment processor for testing."""
    def __init__(self):
        self.processed = []

    def process(self, transaction):
        """Process a payment transaction."""
        if transaction.amount <= 0:
            raise ValueError("Invalid amount")
        transaction.status = "complete"
        self.processed.append(transaction)
        return True


def create_invoice(order, customer):
    """Create an invoice for an order."""
    return {
        "order_id": id(order),
        "customer": customer.name if customer else "Guest",
        "total": order.grand_total(),
        "items": len(order.items)
    }


# Line 250 - Transaction instance used by tests
transaction = Transaction(amount=100.00, currency="USD")
transaction.transaction_id = "txn_001"


def refund_transaction(transaction):
    """Refund a completed transaction."""
    if transaction.status != "complete":
        raise ValueError("Cannot refund incomplete transaction")
    transaction.status = "refunded"
    return -transaction.amount


def get_transaction_history(user_id, limit=10):
    """Get transaction history for a user."""
    return [{"id": f"txn_{i}", "amount": 10 * i} for i in range(limit)]


class MetricsCollector:
    """Mock metrics collector for testing observability."""
    def __init__(self):
        self.metrics = {}

    def record(self, name, value):
        """Record a metric value."""
        if name not in self.metrics:
            self.metrics[name] = []
        self.metrics[name].append(value)

    def get_average(self, name):
        """Get average value for a metric."""
        values = self.metrics.get(name, [])
        return sum(values) / len(values) if values else 0


def compute_statistics(values):
    """Compute basic statistics for a list of values."""
    if not values:
        return {"count": 0, "sum": 0, "avg": 0}
    return {
        "count": len(values),
        "sum": sum(values),
        "avg": sum(values) / len(values),
        "min": min(values),
        "max": max(values)
    }


# Test data for various scenarios
test_items = [
    OrderItem("Widget", 10.00, 2),
    OrderItem("Gadget", 25.00, 1),
    OrderItem("Thing", 5.00, 5),
]

test_order = Order()
for item in test_items:
    test_order.add_item(item)
test_order.customer = user

# Final line - ensures file has substantial content
collector = MetricsCollector()
collector.record("test_metric", 42)
