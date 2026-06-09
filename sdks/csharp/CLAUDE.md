<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## C# SDK Scope

Use this file for work under `sdks/csharp/`.

## C# SDK Overview

`sdks/csharp/` is the Flapjack Search .NET client. It is a drop-in replacement for `Algolia.Search`, supports .NET Standard 2.0 and 2.1, and provides custom-host configuration for self-hosted Flapjack.

Entry points:
- `README.md` owns installation, quick start, migration, and test commands.
- `Flapjack.Search.sln` is the solution entry point.
- `flapjacksearch/` contains client source.
- `tests/` contains SDK tests.

## C# SDK Rules

- Preserve Algolia-compatible request and response semantics while using Flapjack namespaces and package names.
- Prefer edits in transport, configuration, serializer, exceptions, HTTP, utility, and common-model code.
- Treat files with generated-code headers as generated; change their generator inputs instead of hand-editing them.
- Build with `dotnet build Flapjack.Search.sln`.
- Run focused tests with `dotnet test tests/Flapjack.Search.Tests.csproj`.
