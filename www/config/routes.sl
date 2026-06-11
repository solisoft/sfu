# soli-sfu documentation site

get("/", "docs#overview", name: "root")
get("/architecture", "docs#architecture", name: "architecture")
get("/api", "docs#api", name: "api")
get("/tokens", "docs#tokens", name: "tokens")
get("/client", "docs#client", name: "client")
get("/bonfire", "docs#bonfire", name: "bonfire")
get("/ops", "docs#ops", name: "ops")

# Health check endpoint
get("/health", "home#health")
