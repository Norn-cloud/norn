# Parse the public BPIC 2017 log (4TU.ResearchData, CC BY 4.0) into cases.parquet + events.parquet. Runs on Boat.
# Parse BPIC 2017 XES into events.parquet + cases.parquet, then profile.
import gzip, sys, collections
from lxml import etree
import pandas as pd
src = "/home/user/data/raw/f3aec4f7-d52c-4217-82f4-57d719a8298c"
NS = "{http://www.xes-standard.org/}"
def attrs(el):
    d = {}
    for c in el:
        tag = c.tag.replace(NS, "")
        if tag in ("string", "date", "float", "int", "boolean", "id"):
            d[c.get("key")] = c.get("value")
    return d
cases, events = [], []
with gzip.open(src) as f:
    for _, tr in etree.iterparse(f, tag=(NS + "trace", "trace")):
        ca = attrs(tr); cid = ca.get("concept:name")
        cases.append(ca)
        for ev in tr.iter(NS + "event", "event"):
            ea = attrs(ev); ea["case"] = cid; events.append(ea)
        tr.clear()
        while tr.getprevious() is not None: del tr.getparent()[0]
C = pd.DataFrame(cases); E = pd.DataFrame(events)
E["ts"] = pd.to_datetime(E["time:timestamp"], utc=True, format="ISO8601")
C.to_parquet("/home/user/data/cases.parquet"); E.to_parquet("/home/user/data/events.parquet")
print("cases", len(C), "events", len(E)); print("case cols", list(C.columns)); print("event cols", list(E.columns))
print(E["concept:name"].value_counts().to_string())
print(C.describe(include="all").T.to_string()[:3000])
