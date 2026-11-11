app_name = "norn_bridge"
app_title = "Norn Bridge"
app_publisher = "Norn contributors"
app_description = "Generic bridge: Frappe shows Norn-owned objects; the cell decides."
app_license = "Apache-2.0"

# Apply the staged projection bundle as part of `bench migrate`.
after_migrate = ["norn_bridge.sync.after_migrate"]
