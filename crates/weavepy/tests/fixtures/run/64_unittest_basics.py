import unittest


class MathTests(unittest.TestCase):
    def test_addition(self):
        self.assertEqual(1 + 1, 2)

    def test_inequality(self):
        self.assertNotEqual(1, 2)

    def test_truth(self):
        self.assertTrue(bool([1]))
        self.assertFalse(bool([]))

    def test_in(self):
        self.assertIn(1, [1, 2, 3])
        self.assertNotIn(99, [1, 2, 3])

    def test_raises(self):
        with self.assertRaises(ValueError):
            int("nope")

    def test_almost(self):
        self.assertAlmostEqual(0.1 + 0.2, 0.3, places=6)


class StringTests(unittest.TestCase):
    def test_upper(self):
        self.assertEqual("hi".upper(), "HI")


suite = unittest.TestSuite()
suite.addTest(unittest.defaultTestLoader.loadTestsFromTestCase(MathTests))
suite.addTest(unittest.defaultTestLoader.loadTestsFromTestCase(StringTests))

result = unittest.TextTestRunner(verbosity=0, stream=None).run(suite)
print("tests run:", result.testsRun)
print("failures:", len(result.failures))
print("errors:", len(result.errors))
print("was successful:", result.wasSuccessful())
