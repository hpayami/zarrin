# Differential cases

Every `.zr` file here is run through the interpreter and as a native
executable, and the two are required to answer alike: same output, same first
line of diagnostic, same exit status. `../differential.rs` is the test.

These are not examples. `examples/` shows the language off; these are written
to catch the two backends disagreeing, so they lean on the edges — programs
that fail, empty and one-element cases, the smallest and largest integers,
non-ASCII strings, loops that break out of the middle. Every case here found
something once.

A case does not have to succeed, and it does not have to be accepted: a program
both backends refuse in the same words is a program they agree on. The five
`rejected_*` cases are exactly that — each one is a bug both backends share,
kept here so that whichever way it is settled, they settle it together.

To add one, drop a `.zr` file in and run the test. There is nothing to
register and no expected output to write down: the interpreter is the answer.
