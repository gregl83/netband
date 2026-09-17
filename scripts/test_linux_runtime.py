"""Regression checks for the release ELF compatibility gate."""
import unittest

from linux_runtime import audit


class RuntimeTests(unittest.TestCase):
    def check(self, versions="GLIBC_2.17 GLIBC_2.34", libraries=None, interpreter=None,
              machine="Advanced Micro Devices X86-64", target="x86_64-unknown-linux-gnu"):
        libraries = libraries if libraries is not None else ["libc.so.6", "libm.so.6", "libgcc_s.so.1"]
        dynamic = "\n".join(f"(NEEDED) Shared library: [{name}]" for name in libraries)
        interpreter = interpreter or "/lib64/ld-linux-x86-64.so.2"
        return audit(target, f"Machine: {machine}",
                     f"[Requesting program interpreter: {interpreter}]", dynamic, versions)

    def test_accepts_baseline_and_compares_versions_numerically(self):
        self.assertEqual(self.check("GLIBC_2.9 GLIBC_2.35")['glibc_required'], "2.35")

    def test_rejects_newer_glibc(self):
        with self.assertRaisesRegex(ValueError, "glibc"):
            self.check("GLIBC_2.36")

    def test_rejects_private_or_unrecognized_glibc_versions(self):
        for version in ("GLIBC_PRIVATE", "GLIBC_ABI_DT_RELR"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.check(version)

    def test_rejects_unexpected_shared_dependencies(self):
        with self.assertRaisesRegex(ValueError, "libssl"):
            self.check(libraries=["libc.so.6", "libssl.so.3"])

    def test_rejects_missing_elf_information(self):
        with self.assertRaises(ValueError):
            audit("x86_64-unknown-linux-gnu", "", "", "", "")

    def test_rejects_wrong_architecture_and_loader(self):
        with self.assertRaisesRegex(ValueError, "architecture"):
            self.check(machine="AArch64")
        with self.assertRaisesRegex(ValueError, "interpreter"):
            self.check(interpreter="/lib/ld-musl-x86_64.so.1")

    def test_accepts_aarch64(self):
        result = self.check(machine="AArch64", target="aarch64-unknown-linux-gnu",
                            interpreter="/lib/ld-linux-aarch64.so.1")
        self.assertEqual(result['glibc_required'], "2.34")


if __name__ == "__main__":
    unittest.main()
