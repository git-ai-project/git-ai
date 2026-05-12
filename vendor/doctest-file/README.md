Sources doctests from dedicated files into Rustdoc documentation while hiding
select lines.

## Introduction
The purpose of this procedural macro is to make it practical to put doctests
into separate `.rs` files. `include_doctest!()` works like `include_str!()`
while adding the following features:
- Lines can be hidden from rendered documentation by suffixing them with `//`
	- `//{` and `//}` can be used to hide many lines
- The indentation of the included text is normalized (reduced to that of the
  least indented visible line)
- mdBook-style regions delimited by `// ANCHOR: region-name` and `//
  ANCHOR_END: region-name` can be included

This crate has zero dependencies: no `proc_macro2`, no `syn`, no `quote`. It
is also reasonably simple, at just under 400 lines of code.

Example usage:
```rust
/// ```rust
#[doc = doctest_file::include_doctest!("examples/foo.rs")]
/// ```
```

Or, to include only a specific region of a file (see [*Anchors*](#anchors)):
```rust
/// ```rust
#[doc = doctest_file::include_doctest!("examples/foo.rs", region = "bar")]
/// ```
```

## Hiding lines
`include_doctest!()` provides a text editor friendly alternative to Rustdoc's
[`#` syntax] for hiding lines from rendered documentation: simply suffix a
line with `//` to make it invisible. (Under the hood, this is translated to
the `#` syntax used by Rustdoc.) Unlike a `#` at the start of a line, an empty
end-of-line comment is not invalid Rust syntax, so you can benefit from syntax
highlighting and a lack of bogus syntax error messages while writing doctests.
Note that whitespace is ignored during detection of this suffix. Additionally,
if a line only contains whitespace and comments, this syntax will not cause
the line to be hidden.

[`#` syntax]: https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html#hiding-portions-of-the-example

Blocks of lines can also be hidden – this helps work around Rustfmt's
inflexible formatting choices and allows hiding comment-only lines. `//{`
starts a hidden block, which only includes the line it is on if there is
nothing on the line other than that marker (aside from whitespace). `//}` ends
a hidden block while unconditionally hiding the line it's on.

The `hidden` flag can be used to hide everything from an included file (useful
for repetitive boilerplate):
```rust
#[doc = doctest_file::include_doctest!("examples/foo.rs", hidden)]
```

## Indent normalization
`include_doctest!()` will reduce indentation of visible lines such that the
least indented visible line appears completely unindented. This means that you
don't need to bend the indentation rules that are enforced by Rustfmt to get
your doctest looking right in Rustdoc.

Additionally, blank lines at the beginning and at the end of the included code
are removed, also helping interoperability with Rustfmt.

## Anchors
You can designate regions that can be separately included using mdBook-style
anchor syntax:
```rust
// ANCHOR: bar
do_something();
// ANCHOR_END: bar
// ANCHOR: baz
do_another_thing();
// ANCHOR_END: baz
# fn do_something() {}
# fn do_another_thing() {}
```
Those can then be included as follows:
```
/// First, we do a thing:
/// ```rust
#[doc = doctest_file::include_doctest!("examples/foo.rs", region = "bar")]
/// ```
/// Then, we do the other thing:
/// ```rust
#[doc = doctest_file::include_doctest!("examples/foo.rs", region = "baz")]
/// ```
```
In both cases, the entire file will be included into the doctest, but only the
given region will be visible – the rest will be hidden as with `//` or `//{`
and `//}`. This is useful for interspersing code with long-winded explanations
formatted with Markdown, or for breaking down a large example into smaller
sections. You can use the code block types `no_run` and `ignore` on repeat
inclusions of the same file to avoid running the same doctest many times.

# License
`doctest-file` is licensed under the 0-clause BSD license, meaning that it is
impossible to violate the licensing terms. This is functionally equivalent to
the public domain, but is also legal in countries such as Germany.

# Anti-LLM notice
I (Goat), the author of this software, have never used any LLM (large language
model) software in any context for any reason. I will continue to uphold this
in the lack of violent or otherwise unweatherable external pressure mandating
me to do so.

Accordingly, LLMs have not been used in the development of this software,
and LLM-generated code is not present in the source tree. If you desire to
avoid software produced with the use of LLMs, or if you are working under
restrictions that prohibit use of LLM-generated dependencies, this software
can safely be added to your allowlist.

Please note that, while an effort is made to avoid introducing dependencies
containing LLM-generated code and ones known to have been made with the use of
LLMs by their core developers, there are insufficient resources to guarantee a
total lack of such software in the transitive dependency tree. If you discover
use of LLMs upstream of this software, you are urged to report this on the
issue tracker. The offending dependency will be removed within the constraints
of feasibility and maintainer bandwidth.

Additionally, some dependencies form part of the public API of this software.
Those cannot simply be removed without impacting users and thereby stooping
lower than the users of LLMs themselves. For dependencies that are large
and/or part of the public API while not being authored and maintained by me,
an effort is made to gate them behind off-by-default features to minimize the
risk of LLM contamination. Note that this policy predates the LLM pandemic and
is also conductive of proactive prevention of watering hole attacks
([please do not refer to those as "supply-chain attacks"][not-supplier]).

[not-supplier]: https://www.softwaremaxims.com/blog/not-a-supplier

### Why avoid "AI" and its users?
My ethical judgement of LLMs and other forms of so-called AI is the result
of experiencing the "AI" bubble take the world by storm and unmistakably
worsening just about every aspect of human life I care about:

- Machine translation has lowered into the ground the standards for what
  is considered adequate translation. Something that I have bared witness
  to in my own living room is that it is now completely normal for an
  English→Russian translation of a safety manual for a construction crane
  to be a freelance gig with pitiful pay that requires deciphering atrocious
  Chinese→English machine translation and inventing ways of localizing
  terminology that does not exist in the industry – all because it is that
  much cheaper to roll the token casino and have qualified professionals
  take impossible responsibility for the negligence of people outside their
  control. Misleading marketing targeting those underinformed and those eager
  to line their pockets, aided by the [fraudulent benchmarks][pivot-selfpromo]
  with which machine learning companies misrepresent their products, despite
  the levels of conflict of interest with little to no precedent of comparable
  scale in the history of academia being patently obvious, have made this
  method of cutting costs acceptable in general business ethics, even when
  people's lives are on the line. That
  [large language models have turned out to be pretty bad at languages][pivot-duolingo]
  is about as symbolic a wakeup call as it gets.

- Superficiality, obligatoriness, and technique have long been an artificial
  and circularly reasoning benchmark of quality that mainstream society amply
  employs as a weapon of sophism to bash artists of various fields for doing
  things they don't like, but with neural network image diffusion excelling
  at achieving those three qualities and hardly anything else, the "AI" bubble
  confers the strongest push toward conformism and soullessness in art in
  recent times. Utility art is now ugly, utility music is dull, and artists
  have fewer means than ever of convincing the economy that they deserve
  to exist as living beings – all while the supposedly intelligent tools
  replacing their work are so unoriginal that
  [one can distill their output to one of 12 generic recurring images][pivot-templates].
  Other fields of art and creative expression are experiencing similar
  devaluation of authenticity as LLM-generated blog posts designed to be
  skimmed rather than read disgrace the eyes of readers in nerd online spaces
  all while executives of AAA game companies compete with those of Hollywood
  in how much they can cut costs on soulless cash-grab drivel by outsourcing
  more and more to the slop machines.

- The propensity of LLMs and neural network image diffusion to produce content
  that appears natural to users but has absolutely no relation to reality has
  plunged the concept of truth itself into a deepening crisis. Media outlets
  without standards of quality can now fabricate images of objects that do
  not exist and events that never happened at a higher rate than ever before,
  and gullible people are more eager than ever to propagate them as evidence.
  The "AI" boom is a gift not only to the likes of RT and Tsargrad, which are
  professional disinformation outlets catering to a particular audience akin
  to content farms, but also to social media personalities abusing context
  collapse to deliver harmful falsehoods to audiences that would
  otherwise never stumble upon them.
  [Deepfakes in stark violation of consent][pivot-deepfakes],
  [automated libel][pivot-libel],
  [blunt force propaganda][wp-trump-star-wars] – nothing is off the table
  in the intellectual and ethical race to the bottom powered by the slop
  machines.

- Finally, and of most interest to this anti-LLM notice, the effect of LLMs on
  the software industry have been no less negative.
  [Nearly half of all code generated by "AI" has been found to contain security flaws][veracode],
  and [`curl`'s maintainers have experienced first-hand][curl-thousand-slops]
  the shocking spam wave of pseudo-security-research mass-produced by LLMs.
  Social contracts fostering cooperation and reciprocity have been obliterated
  by a typhoon of abuse motivated by financial interest, as website
  administrators are now forced to make choices with no good options: either
  [prevent users who have JavaScript disabled from accessing their website][anubis],
  or
  [suffer outages that prevent *everyone* from accessing their website][gnu-llm-dos].
  Far from all of the kind souls carrying out volunteer work completely for
  free will weather this manufactured storm in the wake of "AI" companies
  ruthlessly robbing our temples of open information, as sifting through spam
  and abuse in a constant state of heightened caution is now a mandatory part
  of the workload.

Yet more evidence for the overwhelming amounts of harm brought upon all of
humankind by the AI pandemic can be found on resources such as the
[AI Incident Database][ai-incident-db] and the
[Wikipedia article on the challenges to ethics of "artificial intelligence"][wp-ai-ethics].
You might also want to take a look at the
[LLM-Afflicted Software][llm-afflicted-software] registry. If you know of
concise and helpful resources that could be additionally linked to by this
notice, do not hesitate to inform me of them.

The bottom line of all this is that those who wish to avoid a fate of drowning
in the world flood of neural network slop have to take it upon themselves
to resist this tide. The manufacture of consent for things that go starkly
against the interests of the people is not a magical process, and it can be
interfered with. A key part of this resistance is vocal rejection – the louder
we cry about the harm the "AI" boom is causing us, the more difficult to
chew we become for the all-devouring worm of venture capital and stock market
hysteria. Just as important if not more important is hitting the perpetrators
where it materially hurts – their coffers. The more difficult it is to use
"AI" tools on account of societal pushback, the less likely people are to
spend their disposable income on subscriptions that finance the perpetual
treadmill of training of models, thereby funding the abuse of our internet
resources, the devaluation of actual intelligent life, and the destruction of
our planet with ever-growing emissions, debasement of otherwise inhabitable
territory by the noise pollution and resource consumption of superfluous data
centers, and a fundamentally destructive trajectory of unbounded growth on a
finitely-sized planet.

The limitations of this methodology are not lost on me, and I certainly do
not believe that a collective boycott of so-called artificial intelligence
is sufficient to steer our world away from the worst possible outcome. Still,
doing what you can to create collective, decentralized, unignorable and
unyielding pushback is a much better alternative to sitting idly with your
hands thrown high up in the air and hopelessly watching all that which makes
life worth living be cast into a planet-sized fire pit. By simply sharing this
message and replicating a zero-tolerance policy against LLMs in the projects
that you own, you will already be doing much more to solve this crisis than
the average person.

If you are an LLM user looking to contribute to this software with the use
of LLM coding assistants or to ask for support in using this software in your
"AI"-powered or "AI"-promoting endeavors, you can hopefully now understand why
I will refuse to cooperate in both of those scenarios.

[wp-trump-star-wars]: https://en.wikipedia.org/wiki/AI_slop#/media/File:AI_Donald_Trump_Star_Wars.jpg
[wp-ai-ethics]: https://en.wikipedia.org/wiki/Ethics_of_artificial_intelligence#Challenges
[pivot-libel]: https://pivot-to-ai.com/2024/08/23/microsoft-tries-to-launder-responsibility-for-copilot-ai-calling-someone-a-child-abuser
[pivot-selfpromo]: https://pivot-to-ai.com/2025/02/25/ai-benchmarks-are-self-promoting-trash-but-regulators-keep-using-them
[pivot-duolingo]: https://pivot-to-ai.com/2025/05/04/duolingo-replaces-its-contractors-with-ai-courses-with-slop
[pivot-templates]: https://pivot-to-ai.com/2025/12/22/ai-image-generators-have-just-12-generic-templates
[pivot-deepfakes]: https://pivot-to-ai.com/2026/01/09/grok-generates-bikini-pics-of-children-uk-us-oddly-powerless
[veracode]: https://www.techradar.com/pro/nearly-half-of-all-code-generated-by-ai-found-to-contain-security-flaws-even-big-llms-affected
[curl-thousand-slops]: https://daniel.haxx.se/blog/2025/07/14/death-by-a-thousand-slops
[anubis]: https://anubis.techaro.lol
[gnu-llm-dos]: https://www.fsf.org/bulletin/2025/spring/defending-savannah-from-ddos-attacks
[ai-incident-db]: https://incidentdatabase.ai
[llm-afflicted-software]: https://codeberg.org/ai-alternatives/llm-afflicted-software
