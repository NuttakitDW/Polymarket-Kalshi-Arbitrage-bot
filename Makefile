.PHONY: run test-arb dryrun db db-cli build clean

# Run the bot normally
run:
	cargo run

# Run with synthetic arb injection for testing
test-arb:
	TEST_ARB=1 RUST_LOG=info cargo run

# Run in dry-run mode (no actual trades)
dryrun:
	DRY_RUN=1 RUST_LOG=info cargo run

# Open database in DB Browser for SQLite (macOS)
db:
	@if [ -f arb.db ]; then \
		open -a "DB Browser for SQLite" arb.db 2>/dev/null || \
		open arb.db 2>/dev/null || \
		echo "DB Browser not found. Install it or use 'make db-cli'"; \
	else \
		echo "arb.db not found. Run the bot first to create it."; \
	fi

# Open database in terminal with sqlite3
db-cli:
	@if [ -f arb.db ]; then \
		sqlite3 arb.db; \
	else \
		echo "arb.db not found. Run the bot first to create it."; \
	fi

# Show database stats
db-stats:
	@if [ -f arb.db ]; then \
		echo "=== Database Stats ==="; \
		sqlite3 arb.db "SELECT 'Markets: ' || COUNT(*) FROM markets;"; \
		sqlite3 arb.db "SELECT 'Arb Snapshots: ' || COUNT(*) FROM arb_snapshots;"; \
		echo ""; \
		echo "=== Recent Arb Opportunities ==="; \
		sqlite3 -header -column arb.db \
			"SELECT datetime(a.timestamp, 'unixepoch', 'localtime') as time, \
			        m.description, a.yes_ask as YES, a.no_ask as NO, \
			        a.total_cost, a.gap_cents as gap, a.profit_per_contract as ppc, a.max_profit_cents as max_profit \
			 FROM arb_snapshots a \
			 JOIN markets m ON a.market_id = m.id \
			 ORDER BY a.timestamp DESC LIMIT 10;"; \
	else \
		echo "arb.db not found. Run the bot first to create it."; \
	fi

# Build release
build:
	cargo build --release

# Clean build artifacts and database
clean:
	cargo clean
	rm -f arb.db
