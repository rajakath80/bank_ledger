.PHONY: start stop logs status clean-data

start:
	@mkdir -p /tmp/demo-ledger-incidents
	@if [ ! -f /tmp/savings-tags.json ]; then \
		cp ledger-tags/examples/savings-tags.json /tmp/savings-tags.json; \
	fi
	@if [ ! -f /tmp/demo-ledger/header.json ]; then \
		cargo run -p ledger-store --example seed_ledger -- /tmp/demo-ledger; \
	fi
	docker compose up -d --build

stop:
	docker compose down

logs:
	docker compose logs -f ledger-dashboard

status:
	docker compose ps

clean-data:
	docker compose down
	rm -rf /tmp/demo-ledger \
	       /tmp/demo-ledger-incidents \
	       /tmp/savings-tags.json
	@echo "cleaned ledger data"