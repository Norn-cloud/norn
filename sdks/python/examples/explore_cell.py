# %% [markdown]
# # Explore a Norn cell from Python
#
# Notebook-style script (`# %%` cells run in Jupyter, VS Code or plain
# Python). It uses only the client generated from the cell's OpenAPI
# document: authenticate, list the cell's definitions, read one object type
# into a pandas dataframe, and follow one object's links.
#
# Run it against a local development host started with `norn dev` in a
# project directory:
#
#     python explore_cell.py /path/to/project
#
# What it cannot do yet is listed in `README.md` next to this file.

# %%
import json
import sys
from pathlib import Path

import pandas as pd

from norn_client import AuthenticatedClient
from norn_client.api.definitions import get_definitions_catalog
from norn_client.api.objects import get_object_links, query_objects
from norn_client.models import Refusal

project = Path(sys.argv[1] if len(sys.argv) > 1 else ".")

# %% [markdown]
# ## Authenticate
#
# `norn dev` writes its address to `.norn/dev.json` and a session token to
# the owner-only file `.norn/dev.token`. The token is the cell's bearer
# credential; every route refuses without it.

# %%
session = json.loads((project / ".norn/dev.json").read_text())
token = (project / ".norn/dev.token").read_text().strip()
scope = session["scope"]
client = AuthenticatedClient(base_url=session["url"], token=token)
where = {"org_id": scope["orgId"], "environment_id": scope["environmentId"]}


def ok(answer):
    """The document, or stop with the cell's refusal."""
    if isinstance(answer, Refusal) or answer is None:
        raise SystemExit(f"refused: {answer}")
    return answer


# %% [markdown]
# ## The cell's definitions
#
# One admitted revision: every artifact the checker admitted, by kind.

# %%
catalog = ok(get_definitions_catalog.sync(client=client, **where))
artifacts = pd.DataFrame(
    [
        {"kind": item.additional_properties.get("kind"), "id": item.additional_properties.get("artifactId")}
        for item in catalog.artifacts
    ]
)
print(f"revision {catalog.version}: {len(artifacts)} artifacts")
print(artifacts.groupby("kind").size().to_string())

# %% [markdown]
# ## Objects into a dataframe
#
# A grid query pages through one type's objects (at most 200 a page); the
# loop follows `next` to the end. Values arrive exactly as the cell stores
# them: decimals stay exact text, so convert them deliberately.

# %%
def objects(object_type: str) -> pd.DataFrame:
    rows, after = [], None
    while True:
        page = ok(
            query_objects.sync(object_type, client=client, limit=200, **where, **({"after": after} if after else {}))
        )
        for record in page.records:
            values = record.values.to_dict() if record.values else {}
            rows.append({"id": record.id, "version": record.version, "withheld": record.withheld, **values})
        after = page.next_  # `next` on the wire; the generator renames it
        if not after:
            return pd.DataFrame(rows)


products = objects("shop.Product")
print(products.to_string(index=False))
lines = objects("shop.OrderLine")
print(lines.to_string(index=False))

# %% [markdown]
# ## Follow one object's links
#
# The links touching one product that the caller may see at both ends.

# %%
sku = products["id"].iloc[0]
links = ok(get_object_links.sync("shop.Product", sku, client=client, **where))
edges = pd.DataFrame([edge.to_dict() for edge in links.items])
print(f"links touching shop.Product/{sku}:")
print(edges.to_string(index=False))
