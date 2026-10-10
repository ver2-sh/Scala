Offline contract fixtures from h2oai/h2o-lightning-4b at
`193ad740925b176a3b70a5a13a7cff2f2fadd01e` (Apache-2.0; LICENSE retained).
The shim and serve configuration are byte-for-byte originals, used only as test
inputs. Source: https://huggingface.co/h2oai/h2o-lightning-4b/tree/193ad740925b176a3b70a5a13a7cff2f2fadd01e

The projection excerpt is from SystemPanic/vllm-windows at
`13e844c86da90c1f96bc516d161ea2a14901ab04`,
`vllm/model_executor/layers/logits_processor.py` (Apache-2.0; VLLM-LICENSE retained).
source-review.json records the full reviewed source hashes and excerpt hash.
Tests use symbolic mocks without tensor code. They establish contract behavior,
not GPU numerical equivalence or native execution qualification.
