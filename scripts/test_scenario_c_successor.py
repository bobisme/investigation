"""Test the cold-successor check without opening an investigation workspace."""
import unittest
from scenario_c_successor import resume


class SuccessorTests(unittest.TestCase):
    def test_narrow_conditional_answer_from_brief(self):
        brief = {"ok": True, "result": "brief", "outcomes": [{"type": "dec",
            "effective_status": {"recorded": "accepted", "state": "conditional"},
            "summary": "Database-backed queue; conditional on zero acknowledged-job loss; reopen at 2500000 jobs/day",
            "evidence_refs": [{"id": "obs_01890f3e-7c00-7000-8000-000000000001", "revision": 1}, {"id": "art_01890f3e-7c00-7000-8000-000000000002", "revision": 1}]}]}
        answer = resume(brief)
        self.assertIn("conditional", answer["conclusion"])
        self.assertIn("loss", answer["gap"])

    def test_missing_gap_cannot_be_filled_from_assumptions(self):
        brief = {"ok": True, "result": "brief", "outcomes": [{"type": "dec",
            "effective_status": {"recorded": "accepted", "state": "conditional"},
            "summary": "Database-backed queue, selected at 2500000 jobs/day"}]}
        with self.assertRaises(ValueError):
            resume(brief)

    def test_proposed_choice_is_not_reported_as_accepted(self):
        brief = {"ok": True, "result": "brief", "outcomes": [{"type": "dec",
            "effective_status": {"recorded": "proposed", "state": "proposed"},
            "summary": "Database conditional on zero loss at 2500000 jobs/day",
            "evidence_refs": [{"id": "obs_01890f3e-7c00-7000-8000-000000000001", "revision": 1}, {"id": "art_01890f3e-7c00-7000-8000-000000000002", "revision": 1}]}]}
        with self.assertRaises(ValueError):
            resume(brief)

    def test_evidence_without_exact_revision_is_rejected(self):
        brief = {"ok": True, "result": "brief", "outcomes": [{"type": "dec",
            "effective_status": {"recorded": "accepted", "state": "conditional"},
            "summary": "Database conditional on zero loss at 2500000 jobs/day",
            "evidence_refs": [{"id": "obs_01890f3e-7c00-7000-8000-000000000001"},
                              {"id": "art_01890f3e-7c00-7000-8000-000000000002", "revision": 1}]}]}
        with self.assertRaises(ValueError):
            resume(brief)


if __name__ == "__main__":
    unittest.main()
