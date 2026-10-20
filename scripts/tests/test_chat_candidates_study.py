"""Snapshot equivalence checks for the opt-in C3 experiment (no live services)."""

import importlib.util
import itertools
import sqlite3
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "study-chat-candidates.py"
SPEC = importlib.util.spec_from_file_location("study", SCRIPT)
study = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(study)


class Candidates(unittest.TestCase):
    def test_grouped_ids_match_history_statuses_and_preserve_chat_order(self):
        db = sqlite3.connect(":memory:")
        self.addCleanup(db.close)
        db.executescript(study.SCHEMA)
        statuses = ["delivered", "cancelled", "queued", "sending", None, 5]
        for i, status in enumerate(statuses):
            chat = {"id": f"c{i}"}
            db.execute("INSERT INTO records VALUES (?,'chats',?,?)", (chat["id"], study.encoded(chat), i))
            for j in range(2):
                message = dict(id=f"m{i}-{j}", chatId=chat["id"], status=status)
                db.execute("INSERT INTO chat_messages VALUES (?,?,?,0)",
                           (message["id"], chat["id"], study.encoded(message)))
        pending = {row[0] for row in db.execute(study.PENDING)}
        chats = [study.json.loads(row[0]) for row in db.execute(study.CHATS)]
        reference = []
        for chat in chats:
            messages = [study.json.loads(row[0]) for row in db.execute(study.MESSAGES, (chat["id"],))]
            if any(m.get("status") in ("queued", "sending") for m in messages):
                reference.append(chat["id"])
        self.assertEqual(pending, {"c2", "c3"})
        self.assertEqual([c["id"] for c in chats if c["id"] in pending], reference)
        self.assertEqual(reference, ["c3", "c2"])

    def test_snapshot_action_and_order_equivalence_matrix(self):
        # The model is independently asserted below; this checks the *filter's*
        # necessary-candidate property across combinations, not native launches.
        rows = []
        statuses = [None, "queued", "running", "succeeded", "failed", "cancelled", "interrupted"]
        for i, (status, paused, lifecycle, pending, wake, flag, busy) in enumerate(itertools.product(
                statuses, [False, True], [None, "active", "trash", "purging"],
                [None, "queued", "sending", "steer", "answer"],
                [None, "due", "future", "observed", "pending"],
                [None, "recoveryPending", "sessionRestartRequested", "cancelledByDeletion"], [False, True])):
            chat = dict(id=f"c{i}", paused=paused, lifecycle=lifecycle)
            run = None if status is None else dict(status=status, chatExecution={"messageId": "turn"})
            if flag in ("sessionRestartRequested", "cancelledByDeletion"):
                chat[flag] = True
            if flag == "recoveryPending" and run is not None:
                run[flag] = True
            if wake:
                chat["wakeup"] = dict(id="wake", messageId="turn", wakeAt=10 if wake == "due" else 200)
                if wake in ("observed", "pending"):
                    chat["wakeup"].update(condition={}, observed=wake == "observed")
            messages = [] if pending is None else [dict(id="message", status="sending" if pending == "sending" else "queued",
                                                       mode="steer" if pending in ("steer", "answer") else "queue")]
            if pending == "answer":
                messages[0]["questionId"] = "question"
            rows.append((chat, run, messages, busy))
        # Reverse order so sorting candidates by DISTINCT chat_id would fail.
        rows.reverse()
        pending_ids = {c["id"] for c, _, messages, _ in rows if messages}
        reference, filtered = [], []
        for chat, run, messages, busy in rows:
            expected = [(chat["id"], a) for a in study.actions(chat, run, messages, busy)]
            reference.extend(expected)
            if study.required(chat, run, pending_ids):
                filtered.extend(expected)
        self.assertEqual(filtered, reference)
        self.assertEqual(len(rows), 11200)

    def test_expected_actions_for_discriminating_branches(self):
        chat = {"id": "chat"}
        run = {"status": "succeeded", "chatExecution": {"messageId": "turn"}}
        queued = {"id": "q", "status": "queued", "mode": "queue"}
        sending = {"id": "s", "status": "sending", "mode": "steer"}
        wake = {"id": "wake", "messageId": "turn", "wakeAt": 10}
        self.assertEqual(study.actions(chat, run, [queued]), [("prepare_message", "q")])
        self.assertEqual(study.actions(chat, run, [sending, queued]), [("requeue", "s"), ("prepare_message", "s")])
        self.assertEqual(study.actions(dict(chat, wakeup=wake), run, [queued]), [("prepare_wakeup", "wake")])
        self.assertEqual(study.actions(dict(chat, wakeup=wake), run, []), [("prepare_wakeup", "wake")])
        self.assertEqual(study.actions(dict(chat, paused=True, wakeup=wake), run, [queued]), [])
        self.assertEqual(study.actions(chat, dict(run, recoveryPending=True), [queued]), [])
        self.assertEqual(study.actions(chat, run, [queued], busy=True), [])
        failed = dict(run, status="failed")
        self.assertEqual(study.actions(dict(chat, wakeup=wake), failed, [sending]),
                         [("cancel_wakeup",), ("requeue", "s"), ("pause",)])
        self.assertEqual(study.actions(dict(chat, paused=True), failed, [sending]), [])
        running = dict(run, status="running")
        self.assertEqual(study.actions(chat, running, [sending]), [("publish_steering", ["s"], [])])
        self.assertEqual(study.actions(chat, running, [sending], busy=True), [("publish_steering", ["s"], [])])
        delivering = dict(running, chatExecution={"messageId": "s"})
        self.assertEqual(study.actions(chat, delivering, [sending]), [("publish_steering", [], [])])
        self.assertEqual(study.actions(dict(chat, paused=True), running, [sending]), [("publish_steering", [], [])])
        answer = dict(sending, questionId="question")
        self.assertEqual(study.actions(dict(chat, paused=True), running, [answer]), [("publish_steering", ["s"], [])])
        self.assertEqual(study.actions(dict(chat, withdrawals=["question"]), running, []),
                         [("publish_steering", [], ["question"])])

    def test_pending_only_is_insufficient_and_snapshot_is_not_a_live_recheck(self):
        chat = {"id": "chat", "wakeup": {"id": "wake", "messageId": "turn", "wakeAt": 10}}
        run = {"status": "succeeded", "chatExecution": {"messageId": "turn"}}
        self.assertNotIn("chat", set())
        self.assertEqual(study.actions(chat, run, []), [("prepare_wakeup", "wake")])
        self.assertTrue(study.required(chat, run, set()))
        idle = {"id": "chat"}
        self.assertFalse(study.required(idle, run, set()))
        # A commit after the grouped snapshot requires a later notify/tick. This
        # experiment intentionally cannot establish live transactional equality.
        self.assertEqual(study.actions(idle, run, [dict(id="new", status="queued")]), [("prepare_message", "new")])


if __name__ == "__main__":
    unittest.main()
