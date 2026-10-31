# Multichannel draft-v1

Portable JSON Schema for the P2 snapshot/receiver/command/event/task boundary and
the native P3 receiver API. This is an intentionally versioned, evolving draft.
The corresponding TypeScript definitions live in
`frontend/src/core/multichannel.ts`; semantics and migration points are documented
in `docs/en/MULTICHANNEL_DRAFT.md` and its Chinese counterpart.

`fixtureReceiverAdapter.ts` creates the development snapshot and owns simulated
state. No fixture here replaces original captured IQ or the production contract.
