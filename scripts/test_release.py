"""Release safety checks; no network requests or repository mutations."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.assets = Path(self.temp.name)
        for name in release.ASSETS:
            (self.assets / name).write_text(name)
        self.sha = "a" * 40

    def prepare(self):
        release.prepare("v1.0.0", self.sha, self.assets)

    @patch("release.gh")
    def test_missing_asset_prevents_remote_changes(self, gh):
        (self.assets / release.ASSETS[0]).unlink()
        with self.assertRaises(ValueError):
            self.prepare()
        gh.assert_not_called()

    @patch("release.gh")
    def test_conflicting_tag_is_rejected(self, gh):
        gh.return_value = json.dumps({"object": {"type": "commit", "sha": "b" * 40}})
        with self.assertRaisesRegex(ValueError, "another commit"):
            self.prepare()
        self.assertEqual(gh.call_count, 1)

    @patch("release.gh")
    def test_public_release_is_never_modified(self, gh):
        gh.side_effect = [json.dumps({"object": {"type": "commit", "sha": self.sha}}),
                          json.dumps([[{"tag_name": "v1.0.0", "draft": False}]])]
        with self.assertRaisesRegex(ValueError, "already published"):
            self.prepare()
        self.assertEqual(gh.call_count, 2)

    @patch("release.verify")
    @patch("release.gh")
    def test_new_tag_and_draft_follow_local_verification(self, gh, verify):
        gh.side_effect = [None, "[[]]", "", "", "", ""]
        events = []
        verify.side_effect = lambda *args: events.append("verify")
        original = gh.side_effect
        def record(*args, **kwargs):
            events.append(args)
            return next(original)
        gh.side_effect = record
        self.prepare()
        create_tag = next(i for i, e in enumerate(events) if isinstance(e, tuple) and "POST" in e)
        self.assertLess(events.index("verify"), create_tag)
        self.assertIn(("release", "create", "v1.0.0", "--verify-tag", "--draft", "--title", "v1.0.0",
                       "--notes", release.DRAFT_NOTES), events)
        self.assertEqual(events[-1], "verify")

    @patch("release.verify")
    @patch("release.gh")
    def test_same_commit_draft_can_be_retried(self, gh, verify):
        gh.side_effect = [json.dumps({"object": {"type": "tag", "sha": "c" * 40}}),
                          json.dumps({"object": {"type": "commit", "sha": self.sha}}),
                          json.dumps([[{"tag_name": "v1.0.0", "draft": True}]]), "", ""]
        self.prepare()
        self.assertFalse(any("create" in call.args or "POST" in call.args for call in gh.call_args_list))
        self.assertEqual(verify.call_count, 2)
        self.assertTrue(any("--clobber" in call.args for call in gh.call_args_list))

    @patch("release.verify", side_effect=ValueError("invalid attestation"))
    @patch("release.gh")
    def test_unverified_assets_never_create_tag(self, gh, verify):
        gh.side_effect = [None, "[[]]"]
        with self.assertRaisesRegex(ValueError, "invalid attestation"):
            self.prepare()
        self.assertEqual(gh.call_count, 2)

    @patch("release.verify")
    @patch("release.gh")
    def test_failed_upload_does_not_report_success(self, gh, verify):
        gh.side_effect = [None, "[[]]", "", "", RuntimeError("upload failed")]
        with self.assertRaisesRegex(RuntimeError, "upload failed"):
            self.prepare()
        self.assertEqual(verify.call_count, 1)

    @patch("release.verify")
    @patch("release.gh")
    def test_downloaded_assets_must_pass_verification(self, gh, verify):
        gh.side_effect = [None, "[[]]", "", "", "", ""]
        verify.side_effect = [None, ValueError("download mismatch")]
        with self.assertRaisesRegex(ValueError, "download mismatch"):
            self.prepare()

    @patch("release.subprocess.run")
    def test_only_explicit_404_counts_as_missing(self, run):
        run.return_value.returncode = 1
        run.return_value.stderr = "gh: Not Found (HTTP 404)"
        self.assertIsNone(release.gh("api", "endpoint", missing_ok=True))
        for error in ["HTTP 403", "HTTP 500", "connection refused"]:
            run.return_value.stderr = error
            with self.assertRaises(RuntimeError):
                release.gh("api", "endpoint", missing_ok=True)

    @patch("release.gh")
    def test_verification_binds_every_asset_to_commit_and_workflow(self, gh):
        for name in release.ASSETS:
            if name.endswith(".sha256"):
                archive = name.removesuffix(".sha256")
                digest = hashlib.sha256((self.assets / archive).read_bytes()).hexdigest()
                (self.assets / name).write_text(f"{digest}  {archive}\n")
        with patch.dict("os.environ", GH_REPO="owner/netband"):
            release.verify(self.assets, self.sha)
        self.assertEqual(gh.call_count, len(release.ASSETS))
        for call in gh.call_args_list:
            self.assertIn(self.sha, call.args)
            self.assertIn("owner/netband/.github/workflows/cd.yml", call.args)
        (self.assets / release.ASSETS[0]).write_text("tampered")
        with self.assertRaisesRegex(ValueError, "checksum"):
            release.verify(self.assets, self.sha)


if __name__ == "__main__":
    unittest.main()
