# Usage:
#   make start     -- build images, seed ledger data if missing, run the dashboard
#   make shutdown  -- stop and remove the dashboard container
#   make logs      -- tail the dashboard's logs
#   make status    -- show what's currently running
#   make clean-data -- wipe the seeded ledger/tags (start from scratch)

LEDGER_DIR      := /tmp/demo-ledger
INCIDENTS_DIR   := /tmp/demo-ledger-incidents
TAGS_FILE       := /tmp/savings-tags.json
OLLAMA_MODEL    := qwen3:8b

GUARDIAN_CMD := docker run --rm -i -e LEDGER_AGENT_ROLE=guardian -v $(LEDGER_DIR):/data:ro ledger-guardian:latest /data
TECHOPS_CMD  := docker run --rm -i -e LEDGER_AGENT_ROLE=techops -v $(LEDGER_DIR):/data:ro -v $(INCIDENTS_DIR):/incidents ledger-techops:latest /data /incidents
FINOPS_CMD   := docker run --rm -i -e LEDGER_AGENT_ROLE=finops -v $(LEDGER_DIR):/data:ro -v $(TAGS_FILE):/tags.json:ro -v /var/run/docker.sock:/var/run/docker.sock ledger-finops:latest /data /tags.json

.PHONY: start shutdown images seed logs status clean-data

## Build every image, seed data if it doesn't exist yet, then run the
## dashboard detached. Safe to run again if it's already up -- it just
## rebuilds images (fast, cached) and leaves an already-running dashboard
## alone rather than erroring.
start: images seed
	@if [ -n "$$(docker ps -q -f name=^ledger-dashboard$$)" ]; then \
		echo "ledger-dashboard is already running -- see 'make status' or 'make logs'"; \
	else \
		docker rm -f ledger-dashboard >/dev/null 2>&1 || true; \
		docker run -d --name ledger-dashboard \
			-p 3000:3000 \
			-e GUARDIAN_CMD='$(GUARDIAN_CMD)' \
			-e TECHOPS_CMD='$(TECHOPS_CMD)' \
			-e FINOPS_CMD='$(FINOPS_CMD)' \
			-e OLLAMA_URL='http://host.docker.internal:11434' \
			-e OLLAMA_MODEL='$(OLLAMA_MODEL)' \
			-e LEDGER_DIR='$(LEDGER_DIR)' \
			-v $(LEDGER_DIR):$(LEDGER_DIR) \
			-v /var/run/docker.sock:/var/run/docker.sock \
			ledger-dashboard:latest; \
		echo "started -- open http://localhost:3000"; \
	fi

## Stop and remove the dashboard container. Sibling agent containers are
## already ephemeral (--rm -i, one per tool call) so there's nothing else
## to clean up here.
shutdown:
	-docker stop ledger-dashboard
	-docker rm ledger-dashboard
	@echo "stopped."

images:
	docker build -f ledger-guardian/Dockerfile  -t ledger-guardian:latest  .
	docker build -f ledger-techops/Dockerfile   -t ledger-techops:latest   .
	docker build -f ledger-finops/Dockerfile    -t ledger-finops:latest    .
	docker build -f ledger-dashboard/Dockerfile -t ledger-dashboard:latest .

## Seed the ledger and tags file only if they don't already exist --
## re-seeding an already-seeded directory corrupts replay (see
## seed_ledger.rs's own guard). Remember: /tmp does not survive a Mac
## reboot, so this will re-seed automatically the first `make start`
## after a restart.
seed:
	@if [ -f $(LEDGER_DIR)/header.json ]; then \
		echo "$(LEDGER_DIR) already seeded, skipping (run 'make clean-data' to reset)"; \
	else \
		cargo run -p ledger-store --example seed_ledger -- $(LEDGER_DIR); \
	fi
	@mkdir -p $(INCIDENTS_DIR)
	@if [ ! -f $(TAGS_FILE) ]; then \
		cp ledger-tags/examples/savings-tags.json $(TAGS_FILE); \
	fi

logs:
	docker logs -f ledger-dashboard

status:
	@docker ps --filter "name=ledger" --format 'table {{.Names}}\t{{.Image}}\t{{.Status}}'

## Wipe seeded data so the next `make start` begins from scratch. Does not
## touch built images or the running container -- run `make shutdown`
## first if you want a truly clean slate.
clean-data:
	rm -rf $(LEDGER_DIR) $(INCIDENTS_DIR) $(TAGS_FILE)
	@echo "cleaned $(LEDGER_DIR), $(INCIDENTS_DIR), $(TAGS_FILE)"