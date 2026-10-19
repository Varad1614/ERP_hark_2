"""Unit tests of the pure helpers; run with `python3 -m unittest test_rows.py` (no paddle needed)."""

import base64
import unittest

import app


def box(text, x0, y0, x1, y1, conf=0.9):
    return {"text": text, "confidence": conf, "x0": x0, "y0": y0, "x1": x1, "y1": y1}


class GroupRows(unittest.TestCase):
    def test_one_printed_row_becomes_one_line(self):
        lines = app.group_rows([
            box("378.00", 300, 402, 360, 422, 0.98),
            box("Grand Total", 10, 400, 120, 420, 0.96),
            box("₹", 260, 401, 272, 421, 0.80),
        ])
        self.assertEqual([l["text"] for l in lines], ["Grand Total ₹ 378.00"])

    def test_rows_top_to_bottom_then_left_to_right(self):
        lines = app.group_rows([
            box("Sub Total", 10, 300, 100, 318),
            box("350.00", 300, 301, 360, 319),
            box("CAFE MADRAS", 80, 10, 260, 40),
            box("CGST 2.5%", 10, 330, 100, 348),
            box("14.00", 300, 331, 350, 349),
        ])
        self.assertEqual(
            [l["text"] for l in lines],
            ["CAFE MADRAS", "Sub Total 350.00", "CGST 2.5% 14.00"],
        )

    def test_slight_skew_stays_in_row_but_next_row_splits(self):
        lines = app.group_rows([
            box("Net Amount", 10, 500, 120, 520),
            box("1,250.00", 280, 506, 360, 526),  # 6 px lower: same row
            box("Thank you", 10, 540, 120, 560),  # next row
        ])
        self.assertEqual([l["text"] for l in lines], ["Net Amount 1,250.00", "Thank you"])

    def test_confidence_is_character_weighted_and_blank_boxes_dropped(self):
        lines = app.group_rows([box("ab", 0, 0, 10, 10, 1.0), box("cd", 20, 0, 30, 10, 0.5),
                                box("   ", 40, 0, 50, 10, 0.1)])
        self.assertEqual(lines, [{"text": "ab cd", "confidence": 0.75}])

    def test_empty(self):
        self.assertEqual(app.group_rows([]), [])


class BoxesFromResult(unittest.TestCase):
    def test_polygons_and_boxes(self):
        res = {
            "rec_texts": ["Total", "", "99.00"],
            "rec_scores": [0.9, 0.1, 0.8],
            "rec_polys": [
                [[10, 10], [60, 10], [60, 30], [10, 30]],
                [[0, 0], [1, 0], [1, 1], [0, 1]],
                [[100, 12], [150, 12], [150, 32], [100, 32]],
            ],
        }
        boxes = app.boxes_from_result(res)
        self.assertEqual([b["text"] for b in boxes], ["Total", "99.00"])
        self.assertEqual(app.group_rows(boxes)[0]["text"], "Total 99.00")
        res2 = {"rec_texts": ["A"], "rec_scores": [0.5], "rec_polys": [], "rec_boxes": [[5, 6, 1, 2]]}
        self.assertEqual(app.boxes_from_result(res2)[0]["x0"], 1.0)


class Helpers(unittest.TestCase):
    def test_fit_long_edge(self):
        self.assertEqual(app.fit_long_edge(4000, 3000), (2000, 1500))
        self.assertEqual(app.fit_long_edge(1000, 3000), (667, 2000))
        self.assertEqual(app.fit_long_edge(800, 600), (800, 600))

    def test_decode_payload(self):
        data = base64.b64encode(b"\xff\xd8hello").decode()
        self.assertEqual(app.decode_payload({"contentType": "image/jpeg", "dataBase64": data}),
                         ("image/jpeg", b"\xff\xd8hello"))
        for bad in ({"contentType": "image/gif", "dataBase64": data},
                    {"contentType": "image/png", "dataBase64": "%%%"},
                    {"contentType": "image/png"}, []):
            with self.assertRaises(ValueError):
                app.decode_payload(bad)

    def test_engine_options(self):
        o = app.engine_options("en")
        self.assertEqual(o["text_detection_model_name"], "PP-OCRv6_small_det")
        self.assertEqual(o["text_recognition_model_name"], "PP-OCRv6_small_rec")
        self.assertNotIn("lang", o)
        self.assertEqual(app.engine_options("devanagari")["lang"], "devanagari")


class StubbedEngine(unittest.TestCase):
    """Reader with a fake engine (needs Pillow + numpy, skipped otherwise)."""

    def test_reader_groups_rows(self):
        try:
            import io

            import numpy  # noqa: F401
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow / numpy not installed")

        class Fake:
            def predict(self, img):
                assert img.shape[2] == 3
                return [{"rec_texts": ["378.00", "Grand Total"], "rec_scores": [0.9, 0.9],
                         "rec_polys": [[[200, 50], [260, 50], [260, 70], [200, 70]],
                                       [[10, 52], [100, 52], [100, 72], [10, 72]]]}]

        buf = io.BytesIO()
        Image.new("RGB", (3000, 1000), "white").save(buf, "PNG")
        lines = app.Reader(Fake()).read("image/png", buf.getvalue())
        self.assertEqual(lines, [{"text": "Grand Total 378.00", "confidence": 0.9}])


if __name__ == "__main__":
    unittest.main()
