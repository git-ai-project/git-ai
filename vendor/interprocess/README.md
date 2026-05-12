[![Crates.io][cratesio-shield]][cratesio] [![Docs.rs][docsrs-shield]][docsrs]
[![Build status][ci-shield]][ci-page] ![Maintenance status][maint-shield]
[![Rust version: 1.75+][msrv-shield]][msrv-blogpost]

[cratesio-shield]: https://img.shields.io/crates/v/interprocess
[docsrs-shield]: https://img.shields.io/badge/documentation-docs.rs-informational
[ci-shield]: https://github.com/kotauskas/interprocess/actions/workflows/checks_and_tests.yml/badge.svg
[maint-shield]: https://img.shields.io/badge/maintenance-passive-green
[msrv-shield]: https://img.shields.io/badge/rust%20version-1.75+-orange

[cratesio]: https://crates.io/crates/interprocess "Interprocess on Crates.io"
[docsrs]: https://docs.rs/interprocess "interprocess on Docs.rs"
[ci-page]: https://github.com/kotauskas/interprocess/actions/workflows/checks_and_tests.yml
[msrv-blogpost]: https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html

Interprocess communication toolkit for Rust programs that aims to expose
as many platform-specific features as possible while maintaining a uniform
interface for all platforms and encouraging portable, correct code.

## Communication primitives
Interprocess provides both OS-specific IPC interfaces and cross-platform
abstractions for them.

##### Cross-platform IPC APIs
- **Local sockets** – similar to TCP sockets, but use filesystem or namespaced
  paths instead of ports on `localhost`, depending on the OS, bypassing the
  network stack entirely; implemented using named pipes on Windows and Unix
  domain sockets on Unix

##### Platform-specific, but present on both Unix-like systems and Windows
- **Unnamed pipes** – anonymous file-like objects for communicating privately
  in one direction, most commonly used to communicate between a child process
  and its parent

##### Unix-only
- **FIFO files** – special type of file which is similar to unnamed pipes but
  exists on the filesystem, often referred to as "named pipes" but completely
  different from Windows named pipes
- *Unix domain sockets* – Interprocess no longer provides those, as they are
  present in the standard library; they are, however, exposed as local sockets

##### Windows-only
- **Named pipes** – resemble Unix domain sockets, use a separate namespace
  instead of on-drive paths

## Asynchronous I/O
Currently, the only supported async runtime is [Tokio]. Local sockets and
Windows named pipes are provided by Interprocess, while Unix domain sockets
are available in Tokio itself.

Support for [`smol`] is planned.

[Tokio]: https://crates.io/crates/tokio
[`smol`]: https://crates.io/crates/smol

## Platform support
Interprocess supports Windows and all generic Unix-like systems. Additionally,
platform-specific extensions are supported on select systems. The policy with
those extensions is to put them behind `#[cfg]` gates and only expose on the
supporting platforms, producing compile errors instead of runtime errors on
platforms that have no support for those features.

Four levels of support (not called *tiers* to prevent confusion with Rust
target tiers, since those work completely differently) are provided by
Interprocess. It would be a breaking change for a platform to be demoted,
although promotions quite obviously can happen as minor or patch releases.

##### Explicit support
*OSes at this level: **Windows**, **Linux**, **macOS***

- Interprocess is guaranteed to compile and succeed in running all tests – it
  would be a critical bug for it not to
- CI, currently provided by GitHub Actions, runs on all of those platforms and
  displays an ugly red badge if anything is wrong on any of those systems
- Certain `#[cfg]`-gated platform-specific features are supported with stable
  public APIs

##### Explicit support with incomplete CI
*OSes at this level: **FreeBSD**, **Android***

- Interprocess is expected to compile and succeed in running all tests – it
  would be a bug for it not to
- GitHub Actions only allows Clippy and Rustdoc to be run for those targets in
  CI (via cross-compilation) due to a lack of native VMs
- Certain `#[cfg]`-gated platform-specific features are supported with stable
  public APIs

##### Explicit support without CI
*OSes at this level: **OpenBSD**, **NetBSD***

- Interprocess is expected to compile and succeed in running all tests – it
  would be a bug for it not to
- Manual testing on local VMs is usually done before every release; no CI
  happens because those targets' standard library `.rlib`s cannot be installed
  via `rustup target add`
- Certain `#[cfg]`-gated platform-specific features are supported with stable
  public APIs

##### Support by association
*OSes at this level: **Dragonfly BSD**, **Redox**, **Fuchsia**, **iOS**, **tvOS**, **watchOS***

- Interprocess is expected to compile and succeed in running all tests – it
  would be a bug for it not to
- No manual testing is performed, and CI is unavailable because GitHub Actions
  does not provide it
- Certain `#[cfg]`-gated platform-specific features that originate from other
  platforms are supported with stable public APIs because they behave here
  identically to how they do on an OS with a higher support level

##### Assumed support
*OSes at this level: POSIX-conformant `#[cfg(unix)]` systems not listed above for which the `libc`
crate compiles*

- Interprocess is expected to compile and succeed in running all tests – it
  would be a bug for it not to
- Because this level encompasses a practically infinite amount of systems, no
  manual testing or CI can exist

## Feature gates
- **`tokio`**, *off* by default – enables the [Tokio] variants of IPC
  primitives (where applicable).

# License
`interprocess` is dual-licensed, at your choice, under the 0-clause BSD
license or the Apache 2.0 license. It is impossible to violate the licensing
terms, as the 0-clause BSD license has none. This is functionally equivalent
to the public domain, but is also legal in countries such as Germany, which
prohibit authors from placing their works in the public domain voluntarily.
The Apache 2.0 licensing does not restrict the users of this software in any
way in this case, but instead only confers a patent grant that protects the
user from patent trolling on my part.

Despite the lack of an attribution clause in the license, I would appreciate
if you gave credit to me when using this software in your own work if you
believe that people deserve to know of it, as a matter of goodwill rather than
legal obligation.

## Why choose this license?
I have long ceased to believe that intellectual property as enforced by
the law is a greater force for software freedom than it is an impediment
to individual developers building freedom-respecting software on top of the
work of others while trying to get by in this increasingly tiresome world
we live in. A fact of life that software freedom ideologues of the less
grounded-in-material-reality sort like to overlook is that, while enforcement
of copyright law is equal (enabling the concept of copyleft and things of
that nature), it is anything but equitable.

Put simply, the rationale for this licensing arrangement is that it would
be thoroughly uncivilized of me to threaten to unleash state violence on
those who have neglected to give me a shoutout. Furthermore, had I placed a
license with an attribution clause (such as the MIT license) on this software,
I would, realistically speaking, never enforce it, as I lack the legal
knowledge, capacity to afford a lawyer, and lust for human suffering necessary
to file a lawsuit of this kind.

My use of the 0-clause BSD license is an act of radical honesty about my
relationship with copyright law and a removal of a-priori-empty threats that
would serve other open-source developers well to imitate. **Say no to licenses
and yes to norms.**

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
([please do not refer to those as "supply-chain attacks"][no-supply-chain]).

[no-supply-chain]: https://iliana.fyi/blog/software-supply-chain/

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
  terminology that does not exist in the industry – all because it is that
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

*P.S.:* If you're wondering where the pervasive en dashes preceded by no-break
spaces come from in my writing, I input those with the help of the Compose
key, which is an input feature available on both Linux (natively in X-Windows
and Wayland) and Windows (via [WinCompose]). Contrary to popular belief, usage
of aesthetically pleasing and well-behaved typography beyond the bounds of
ASCII is not exclusive to LLMs: you can have your fancy dashes, and more –
superscript digits, currency signs, Greek letters, numerous mathematical
symbols, you name it – with any regular keyboard and only a tiny amount of
additional system configuration.

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
[WinCompose]: https://github.com/samhocevar/wincompose
