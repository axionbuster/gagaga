# Manual Testing Stuff

## Outside or Inside?

1. Copy test picture `wpbaldeagle.jpg` into `src/`.
2. Run server using `RUST_LOG=DEBUG cargo run -- src`. That specifies `src/`
as the root directory.
3. Check that two pictures of the same name exist: one at
`(repo root)/wpbaldeagle.jpg`, and the other (copy) at
`(repo root)/src/wpbaldeagle.jpg`.
4. In the browser, try to access `/root/wpbaldeagle.jpg`. This should
route to the one inside `src/`.
5. Check that the routed picture is inside `src/`.
6. Check that the program resolves correctly.
