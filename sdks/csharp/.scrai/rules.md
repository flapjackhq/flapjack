## C# SDK Rules

- Preserve Algolia-compatible request and response semantics while using Flapjack namespaces and package names.
- Prefer edits in transport, configuration, serializer, exceptions, HTTP, utility, and common-model code.
- Treat files with generated-code headers as generated; change their generator inputs instead of hand-editing them.
- Build with `dotnet build Flapjack.Search.sln`.
- Run focused tests with `dotnet test tests/Flapjack.Search.Tests.csproj`.
