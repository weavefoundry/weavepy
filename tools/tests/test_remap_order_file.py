"""Keep linker order remapping conservative across Rust symbol formats."""

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

TOOL = Path(__file__).resolve().parents[2] / "scripts" / "remap-order-file.py"
spec = importlib.util.spec_from_file_location("remap_order_file", TOOL)
remapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remapper)


class RemapTests(unittest.TestCase):
    def test_legacy_trait_escapes_match_v0(self):
        legacy = "_$LT$weavepy_vm..object..Object$u20$as$u20$core..clone..Clone$GT$::clone::h1234567890abcdef"
        v0 = "<weavepy_vm::object::Object as core::clone::Clone>::clone"
        self.assertEqual(remapper.canonical_name(legacy), v0)

    def test_inherent_impl_keeps_generic_arguments(self):
        self.assertEqual(
            remapper.canonical_name("<crate::Type<u64>>::get"),
            "crate::Type<u64>::get",
        )
        self.assertNotEqual(
            remapper.canonical_name("<crate::Type<u64>>::get"),
            remapper.canonical_name("<crate::Type<i64>>::get"),
        )

    def test_exact_symbol_wins_over_ambiguous_demangled_name(self):
        result, counts = remapper.remap(
            ["first"], ["first", "second"], {}, ["name"], ["name", "name"]
        )
        self.assertEqual(result, ["first"])
        self.assertEqual(counts["exact"], 1)

    def test_ambiguous_instantiations_are_not_guessed(self):
        result, counts = remapper.remap(
            ["old"], ["one", "two"], {}, ["generic"], ["generic", "generic"]
        )
        self.assertEqual(result, [])
        self.assertEqual(counts["ambiguous"], 1)

    def test_retains_old_platform_symbols_in_first_use_order(self):
        result, counts = remapper.remap(
            ["old-a", "old-b", "old-a"], ["new-a"], {},
            ["a", "b", "a"], ["a"], retain_originals=True,
        )
        self.assertEqual(result, ["old-a", "new-a", "old-b"])
        self.assertEqual(counts["missing"], 1)

    def test_crate_pairing_uses_name_and_leaves_ambiguity_alone(self):
        source = ["Csold_3foo", "Csdup_3bar", "Csother_3bar"]
        target = ["Csnew_3foo", "Csnext_3bar", "Cslast_3bar"]
        mapping = remapper.crate_mapping(source, target)
        self.assertEqual(mapping, {("old", "foo"): "new"})
        result, counts = remapper.remap(
            ["Csold_3foo", "Csold_3bar"], ["Csnew_3foo", "Csnew_3bar"], mapping,
            ["source-foo", "source-bar"], ["target-foo", "target-bar"],
        )
        self.assertEqual(result, ["Csnew_3foo"])
        self.assertEqual(counts["exact"], 1)

    def test_no_matches_preserves_the_original_file(self):
        with tempfile.TemporaryDirectory() as directory:
            order = Path(directory) / "input.order"
            original = "# Keep this file if remapping fails.\nmissing\n"
            order.write_text(original)
            with (
                patch.object(sys, "argv", ["remap-order-file", str(order), "old", "new"]),
                patch.object(remapper, "symbols", return_value=["defined"]),
                patch.object(remapper, "demangle", side_effect=lambda names: names),
            ):
                with self.assertRaisesRegex(SystemExit, "no ordered symbols matched"):
                    remapper.main()
            self.assertEqual(order.read_text(), original)


if __name__ == "__main__":
    unittest.main()
