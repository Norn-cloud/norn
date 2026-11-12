doc_events = {
    "Invoice": {"on_update": "demo.api.on_update", "validate": ["demo.api.shared_wildcard", "demo.api.shared_unknown"]},
    "*": {"on_update": "demo.api.shared_wildcard"},
    "Missing": {"on_update": "demo.api.shared_unknown"},
}
scheduler_events = {"daily": ["demo.api.daily"]}
permission_query_conditions = {"Invoice": "demo.api.permission"}
has_permission = {"Invoice": "demo.api.has_permission"}
doc_events.update(extra_events)
