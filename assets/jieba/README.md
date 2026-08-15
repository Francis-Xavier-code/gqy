# Jieba dictionary

`dict.txt` is the default dictionary from `jieba-rs` 0.7.4. Miyu converts it
to a compact read-only FST at build time and uses the same frequencies for
Chinese query segmentation without constructing Jieba's mutable in-memory
dictionary.

The dictionary and `jieba-rs` are distributed under the MIT license. The
license text is stored in `LICENSE` beside this file.
