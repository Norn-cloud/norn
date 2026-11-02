"""WellDB's planned entry cannot weaken the existing obligation evidence bar."""
import copy
import json
from pathlib import Path
import unittest

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[2]
PACK = ROOT / "verification/stripmine-2026-08-22"
DOCUMENT = json.loads((PACK / "obligations.json").read_text())
VALIDATOR = Draft202012Validator(json.loads((PACK / "schemas/obligation-registry.schema.json").read_text()))
WELLDB = "INV-WELLDB-ORACLE-PARITY"


class WellDBEnrollmentTests(unittest.TestCase):
    def test_actual_registry_admits_only_a_planned_welldb_entry_without_evidence(self):
        VALIDATOR.validate(DOCUMENT)
        entry = next(item for item in DOCUMENT["obligations"] if item["id"] == WELLDB)
        for field, value in [("status", "implemented"), ("status", "proved"),
                             ("sources", DOCUMENT["obligations"][0]["sources"]),
                             ("checkers", DOCUMENT["obligations"][0]["checkers"])]:
            changed = copy.deepcopy(DOCUMENT)
            next(item for item in changed["obligations"] if item["id"] == entry["id"])[field] = value
            with self.subTest(field=field, value=value):
                self.assertFalse(VALIDATOR.is_valid(changed))

    def test_every_existing_obligation_keeps_its_evidence_and_status_requirements(self):
        tested = 0
        for index, item in enumerate(DOCUMENT["obligations"]):
            if item["id"] == WELLDB:
                continue
            for field, value in [("sources", []), ("checkers", []), ("status", "planned"), ("domain", "welldb")]:
                changed = copy.deepcopy(DOCUMENT)
                changed["obligations"][index][field] = value
                with self.subTest(obligation=item["id"], field=field):
                    self.assertFalse(VALIDATOR.is_valid(changed))
                tested += 1
        self.assertEqual(tested, 27 * 4)


if __name__ == "__main__":
    unittest.main()
