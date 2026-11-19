"""Exercise release decisions with local files and mocked external operations."""
from contextlib import contextmanager, chdir, redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import tarfile
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import bump_version as bump
import publish_release as publish


class VersionTests(unittest.TestCase):
    def test_stable_bumps(self):
        expected = {"none": "0.5.0", "patch": "0.5.1", "minor": "0.6.0", "major": "1.0.0"}
        for kind, version in expected.items():
            with self.subTest(kind=kind):
                self.assertEqual(bump.next_version("0.5.0", kind), version)

    def test_invalid_version_or_bump_is_rejected(self):
        for version in ("v0.5.0", "0.2", "00.5.0", "0.5.0-rc.1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                bump.next_version(version, "patch")
        with self.assertRaises(ValueError):
            bump.next_version("0.5.0", "release")

    def test_only_package_version_changes(self):
        source = '[package]\nname = "zterm"\nversion = "0.5.0"\n[dependencies]\nexample = { version = "0.5.0" }\n'
        expected = source.replace('version = "0.5.0"\n', 'version = "0.5.1"\n', 1)
        self.assertEqual(bump.replace_package_version(source, "0.5.1"), expected)

    def test_missing_or_duplicate_package_version_is_rejected(self):
        for source in ('[dependencies]\na = "1"\n', '[package]\nname = "zterm"\n',
                       '[package]\nversion = "1.0.0"\nversion = "2.0.0"\n'):
            with self.subTest(source=source), self.assertRaises(ValueError):
                bump.replace_package_version(source, "0.5.1")


    def test_version_bump_populates_cache_before_changing_manifest(self):
        with TemporaryDirectory() as directory, chdir(directory):
            original = '[package]\nname = "zterm"\nversion = "0.5.0"\n'
            Path("Cargo.toml").write_text(original)
            Path("Cargo.lock").write_text('version = 4\n[[package]]\nname = "zterm"\nversion = "0.5.0"\n')
            calls = []

            def cargo(command, **kwargs):
                calls.append(command)
                if command == ["cargo", "fetch", "--locked"]:
                    self.assertEqual(Path("Cargo.toml").read_text(), original)
                elif command == ["cargo", "generate-lockfile", "--offline"]:
                    self.assertEqual(calls[0], ["cargo", "fetch", "--locked"])
                    Path("Cargo.lock").write_text('version = 4\n[[package]]\nname = "zterm"\nversion = "0.5.1"\n')
                else:
                    raise AssertionError(command)

            with patch("sys.argv", ["bump_version.py", "patch"]), \
                 patch.object(bump.subprocess, "run", side_effect=cargo), \
                 patch.object(bump.os, "environ", {}), redirect_stdout(io.StringIO()):
                bump.main()
            self.assertIn('version = "0.5.1"', Path("Cargo.toml").read_text())
            self.assertEqual(len(calls), 2)

    def test_fetch_failure_keeps_manifest_and_lockfile_unchanged(self):
        with TemporaryDirectory() as directory, chdir(directory):
            original = '[package]\nname = "zterm"\nversion = "0.5.0"\n'
            lock = 'version = 4\n[[package]]\nname = "zterm"\nversion = "0.5.0"\n'
            Path("Cargo.toml").write_text(original)
            Path("Cargo.lock").write_text(lock)
            failure = bump.subprocess.CalledProcessError(101, ["cargo", "fetch", "--locked"])
            with patch("sys.argv", ["bump_version.py", "patch"]), \
                 patch.object(bump.subprocess, "run", side_effect=failure), \
                 patch.object(bump.os, "environ", {}):
                with self.assertRaises(bump.subprocess.CalledProcessError):
                    bump.main()
            self.assertEqual(Path("Cargo.toml").read_text(), original)
            self.assertEqual(Path("Cargo.lock").read_text(), lock)

class RegistryTests(unittest.TestCase):
    def test_correct_sparse_index_and_version(self):
        checksum = "a" * 64
        body = json.dumps({"vers": "0.5.0", "cksum": checksum, "yanked": False}).encode()
        with patch.object(publish, "read_url", return_value=body) as read:
            self.assertEqual(publish.registry_checksum("0.5.0"), checksum)
            read.assert_called_once_with("https://index.crates.io/zt/er/zterm")

    def test_missing_version_is_unpublished(self):
        for body in (None, b'{"vers":"0.1.1","cksum":"unused"}'):
            with self.subTest(body=body), patch.object(publish, "read_url", return_value=body):
                self.assertIsNone(publish.registry_checksum("0.5.0"))

    def test_invalid_existing_record_is_rejected(self):
        good = {"vers": "0.5.0", "cksum": "a" * 64, "yanked": False}
        bodies = [
            json.dumps({**good, "yanked": True}).encode(),
            json.dumps({**good, "cksum": "invalid"}).encode(),
            (json.dumps(good) + "\n" + json.dumps(good)).encode(),
        ]
        for body in bodies:
            with self.subTest(body=body), patch.object(publish, "read_url", return_value=body):
                with self.assertRaises(ValueError):
                    publish.registry_checksum("0.5.0")


class PublishTests(unittest.TestCase):
    @contextmanager
    def candidate(self, registry, *, tag_exists=False, tag_matches=True,
                  package_matches=True, dirty=False):
        candidate = "a" * 40
        events = []
        with TemporaryDirectory() as directory, chdir(directory):
            Path("Cargo.toml").write_text('[package]\nname = "zterm"\nversion = "0.5.0"\n')
            crate = Path("target/package/zterm-0.5.0.crate")
            crate.parent.mkdir(parents=True)
            vcs = {"git": {"sha1": candidate if package_matches else "b" * 40,
                           "dirty": dirty}, "path_in_vcs": ""}
            data = json.dumps(vcs).encode()
            with tarfile.open(crate, "w:gz") as archive:
                member = tarfile.TarInfo("zterm-0.5.0/.cargo_vcs_info.json")
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
            checksum = hashlib.sha256(crate.read_bytes()).hexdigest()
            environment = {
                "VERSION": "0.5.0", "CANDIDATE_SHA": candidate,
                "GITHUB_REPOSITORY": "LIghtJUNction/zterm",
                "CARGO_REGISTRY_TOKEN": "mocked-without-reading-environment",
                "GH_TOKEN": "mocked-without-reading-environment",
            }

            def output(*command):
                if command == ("git", "rev-parse", "HEAD"):
                    return candidate
                if command == ("git", "status", "--porcelain"):
                    return ""
                if command == ("git", "rev-parse", "refs/tags/v0.5.0^{}"):
                    return candidate if tag_matches else "b" * 40
                raise AssertionError(command)

            def run(*command):
                events.append(command)

            def process(command, **kwargs):
                events.append(tuple(command))
                if command[:2] == ["git", "show-ref"]:
                    return SimpleNamespace(returncode=0 if tag_exists else 1)
                if command == ["cargo", "publish", "--locked"]:
                    return SimpleNamespace(returncode=0)
                raise AssertionError(command)

            answers = iter(registry)
            def registry_checksum(version):
                self.assertEqual(version, "0.5.0")
                answer = next(answers)
                events.append(("registry", answer))
                return checksum if answer == "matching" else answer

            with patch.object(publish, "require_environment", side_effect=environment.__getitem__), \
                 patch.object(publish, "output", side_effect=output), \
                 patch.object(publish, "run", side_effect=run), \
                 patch.object(publish.subprocess, "run", side_effect=process), \
                 patch.object(publish, "registry_checksum", side_effect=registry_checksum), \
                 patch.object(publish, "finish_github_release",
                              side_effect=lambda *args: events.append(("github-release",))), \
                 patch.object(publish.time, "sleep"), redirect_stdout(io.StringIO()):
                yield events

    def test_registry_confirmation_precedes_public_tag_and_release(self):
        with self.candidate([None, "matching"]) as events:
            publish.main()
        push = events.index(("git", "push", "origin", "refs/tags/v0.5.0"))
        self.assertLess(events.index(("registry", "matching")), push)
        self.assertLess(push, events.index(("github-release",)))
        self.assertIn(("python3", "tools/check_package.py"), events)
        self.assertIn(("cargo", "publish", "--locked"), events)

    def test_matching_existing_version_resumes_without_republishing(self):
        with self.candidate(["matching"], tag_exists=True) as events:
            publish.main()
        self.assertNotIn(("cargo", "publish", "--locked"), events)
        self.assertFalse(any(event[:2] == ("git", "tag") for event in events))
        self.assertIn(("github-release",), events)

    def test_mismatched_registry_or_tag_stops_before_publication(self):
        cases = [(["b" * 64], {}), ([None], {"tag_exists": True, "tag_matches": False})]
        for registry, options in cases:
            with self.subTest(options=options), self.candidate(registry, **options) as events:
                with self.assertRaises(ValueError):
                    publish.main()
            self.assertNotIn(("cargo", "publish", "--locked"), events)
            self.assertNotIn(("git", "push", "origin", "refs/tags/v0.5.0"), events)
            self.assertNotIn(("github-release",), events)

    def test_package_identity_must_match_clean_candidate(self):
        for options in ({"package_matches": False}, {"dirty": True}):
            with self.subTest(options=options), self.candidate([], **options) as events:
                with self.assertRaises(ValueError):
                    publish.main()
            self.assertNotIn(("cargo", "publish", "--locked"), events)
            self.assertNotIn(("github-release",), events)

    def test_unconfirmed_registry_leaves_tag_unpublished(self):
        with self.candidate([None] * 13) as events:
            with self.assertRaises(RuntimeError):
                publish.main()
        self.assertNotIn(("git", "push", "origin", "refs/tags/v0.5.0"), events)
        self.assertNotIn(("github-release",), events)


if __name__ == "__main__":
    unittest.main()
