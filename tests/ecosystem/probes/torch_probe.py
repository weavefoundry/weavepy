"""Ecosystem probe: torch CPU capstone (RFC 0076 WS5) — the heaviest
wheel on PyPI and the largest C-API consumer in the matrix. Scope per
the RFC: tensor construction + matmul cross-checked against numpy, an
autograd `backward()` gradient check, a three-epoch MLP training loop
on synthetic data asserting monotone loss, a `state_dict()` save/load
round-trip, and a `DataLoader` with `num_workers=2` (the
multiprocessing leg). `torch.compile` is out of scope — it requires a
host toolchain and is gated upstream.

Everything lives under the `__main__` guard: the DataLoader stage
spawns worker processes, and on macOS the spawn start method re-imports
this module in each child — unguarded top-level work would recurse
(same requirement as CPython)."""

import faulthandler
import io
import sys

faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


def main() -> None:
    stage("import")
    import numpy as np
    import torch
    import torch.nn as nn

    torch.manual_seed(0)

    # --- tensor construction + matmul vs numpy ---------------------------------
    stage("matmul-vs-numpy")
    a = torch.arange(12, dtype=torch.float64).reshape(3, 4)
    b = torch.arange(8, dtype=torch.float64).reshape(4, 2)
    prod = a @ b
    np_prod = a.numpy() @ b.numpy()
    assert prod.shape == (3, 2), prod.shape
    assert np.allclose(prod.numpy(), np_prod), (prod, np_prod)

    # numpy interop is bidirectional: from_numpy shares memory.
    arr = np.ones((2, 3), dtype=np.float32)
    t = torch.from_numpy(arr)
    arr[0, 0] = 7.0
    assert t[0, 0].item() == 7.0, t

    # --- autograd gradient check ------------------------------------------------
    stage("autograd")
    x = torch.tensor([2.0, 3.0], requires_grad=True)
    y = (x**3).sum()  # dy/dx = 3x^2
    y.backward()
    assert torch.allclose(x.grad, torch.tensor([12.0, 27.0])), x.grad

    # --- three-epoch MLP training loop, monotone loss ----------------------------
    stage("train-mlp")
    n = 256
    inputs = torch.randn(n, 8)
    targets = (inputs.sum(dim=1, keepdim=True) > 0).float()
    model = nn.Sequential(nn.Linear(8, 16), nn.ReLU(), nn.Linear(16, 1))
    opt = torch.optim.Adam(model.parameters(), lr=0.05)
    loss_fn = nn.BCEWithLogitsLoss()
    losses = []
    for epoch in range(3):
        epoch_loss = 0.0
        for i in range(0, n, 32):
            batch_x, batch_y = inputs[i : i + 32], targets[i : i + 32]
            opt.zero_grad()
            loss = loss_fn(model(batch_x), batch_y)
            loss.backward()
            opt.step()
            epoch_loss += loss.item()
        losses.append(epoch_loss)
    assert losses[2] < losses[0], losses

    # --- state_dict save/load round-trip -----------------------------------------
    stage("state-dict")
    buf = io.BytesIO()
    torch.save(model.state_dict(), buf)
    buf.seek(0)
    clone = nn.Sequential(nn.Linear(8, 16), nn.ReLU(), nn.Linear(16, 1))
    clone.load_state_dict(torch.load(buf, weights_only=True))
    with torch.no_grad():
        probe_in = torch.randn(4, 8)
        assert torch.allclose(model(probe_in), clone(probe_in))

    # --- DataLoader with worker processes (the multiprocessing leg) --------------
    stage("dataloader-workers")
    from torch.utils.data import DataLoader, TensorDataset

    ds = TensorDataset(inputs, targets)
    loader = DataLoader(ds, batch_size=32, num_workers=2, shuffle=False)
    total = 0
    first_batch = None
    for bx, by in loader:
        if first_batch is None:
            first_batch = bx
        total += bx.shape[0]
    assert total == n, total
    assert torch.equal(first_batch, inputs[:32]), "worker batches out of order"

    stage("done")
    print("torch ok", torch.__version__)


if __name__ == "__main__":
    main()
