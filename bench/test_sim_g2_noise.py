"""The readiness-doc noise table is reproducible: same seed, same numbers."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sim_g2_noise as sim  # noqa: E402


class Deterministic(unittest.TestCase):
    def test_same_seed_same_table_and_a_different_seed_differs(self):
        self.assertEqual(sim.table(300, 265), sim.table(300, 265))
        self.assertNotEqual(sim.table(300, 265), sim.table(300, 266))

    def test_a_quiet_host_decides_and_a_noisy_one_does_not(self):
        quiet, noisy = sim.table(300, 265)[0], sim.table(300, 265)[-1]
        self.assertGreater(quiet[2]["PASS"], 0.9)   # sigma 0.005, macOS
        self.assertLess(noisy[2]["PASS"], 0.1)      # sigma 0.2, macOS


if __name__ == "__main__":
    unittest.main()
