import ts from "typescript";
import type { ModuleSourceResolver } from "./spec-module-sources";
import {
  CANONICAL_VECTOR_FIXTURE_OPERATIONS,
  DEFAULT_EXPORT,
  NEGATIVE_CONTROL_ENVIRONMENT_CHECK,
  NEGATIVE_CONTROL_ENVIRONMENT_CHECK_MODULE,
  type CallableDefinition,
  expressionReferenceName,
  hasDefaultExportModifier,
  hasExportModifier,
  parseSource,
  propertyName,
} from "./vector-capability-ast";

/**
 * Resolves the callable definitions a spec can reach — locally declared functions, object
 * literal methods, and helpers pulled in through named/default/namespace imports and barrel
 * re-exports — so guard collectors can follow a call to the body that actually runs. Only
 * bindings that resolve to the canonical `api-helpers`/`chat-api-helpers` owners carry a
 * `canonicalVectorFixtureOperationName`, which is what stops a same-named spoof from passing.
 */

function isCanonicalNegativeControlEnvironmentCheckDefinition(
  filePath: string,
  name: string,
): boolean {
  return (
    name === NEGATIVE_CONTROL_ENVIRONMENT_CHECK &&
    filePath
      .replace(/\\/g, "/")
      .endsWith(`/${NEGATIVE_CONTROL_ENVIRONMENT_CHECK_MODULE}`)
  );
}

function canonicalVectorFixtureOperationName(
  filePath: string,
  name: string,
): string | undefined {
  const normalizedPath = filePath.replace(/\\/g, "/");
  return normalizedPath.endsWith("/tests/fixtures/api-helpers.ts") ||
    normalizedPath.endsWith("/tests/fixtures/chat-api-helpers.ts")
    ? CANONICAL_VECTOR_FIXTURE_OPERATIONS.find(
        (operation) => operation === name,
      )
    : undefined;
}

function callableDefinitionForImport(
  definition: CallableDefinition,
  scopeDefinitions: Map<string, CallableDefinition>,
): CallableDefinition {
  return {
    ...definition,
    scopeDefinitions: definition.scopeDefinitions ?? scopeDefinitions,
  };
}

function registerDefinitionAlias(
  definitions: Map<string, CallableDefinition>,
  aliasName: string,
  referencedName: string,
): void {
  for (const [name, definition] of [...definitions]) {
    if (name === referencedName || name.startsWith(`${referencedName}.`)) {
      definitions.set(
        `${aliasName}${name.slice(referencedName.length)}`,
        definition,
      );
    }
  }
}

function registerObjectLiteralCallableDefinitions(
  definitions: Map<string, CallableDefinition>,
  objectName: string,
  objectLiteral: ts.ObjectLiteralExpression,
): void {
  for (const property of objectLiteral.properties) {
    if (ts.isShorthandPropertyAssignment(property)) {
      registerDefinitionAlias(
        definitions,
        `${objectName}.${property.name.text}`,
        property.name.text,
      );
      continue;
    }
    if (ts.isMethodDeclaration(property) && propertyName(property.name)) {
      definitions.set(`${objectName}.${propertyName(property.name)}`, {
        node: property,
        isCanonicalNegativeControlEnvironmentCheck: false,
        canonicalVectorFixtureOperationName: undefined,
        isExported: false,
      });
      continue;
    }
    if (!ts.isPropertyAssignment(property)) {
      continue;
    }
    const memberName = propertyName(property.name);
    if (!memberName) {
      continue;
    }
    const qualifiedMemberName = `${objectName}.${memberName}`;
    if (
      ts.isArrowFunction(property.initializer) ||
      ts.isFunctionExpression(property.initializer)
    ) {
      definitions.set(qualifiedMemberName, {
        node: property.initializer,
        isCanonicalNegativeControlEnvironmentCheck: false,
        canonicalVectorFixtureOperationName: undefined,
        isExported: false,
      });
    } else if (ts.isObjectLiteralExpression(property.initializer)) {
      registerObjectLiteralCallableDefinitions(
        definitions,
        qualifiedMemberName,
        property.initializer,
      );
    } else {
      const referencedName = expressionReferenceName(property.initializer);
      if (referencedName) {
        registerDefinitionAlias(
          definitions,
          qualifiedMemberName,
          referencedName,
        );
      }
    }
  }
}

function registerObjectCallableDefinitions(
  sourceFile: ts.SourceFile,
  definitions: Map<string, CallableDefinition>,
): void {
  function visit(node: ts.Node): void {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.initializer &&
      ts.isObjectLiteralExpression(node.initializer)
    ) {
      registerObjectLiteralCallableDefinitions(
        definitions,
        node.name.text,
        node.initializer,
      );
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
}

function reExportedCallableDefinitions(
  statement: ts.ExportDeclaration,
  filePath: string,
  resolveModuleSource: ModuleSourceResolver | undefined,
  visitedFilePaths: Set<string>,
): Map<string, CallableDefinition> | undefined {
  if (
    !resolveModuleSource ||
    !statement.moduleSpecifier ||
    !ts.isStringLiteralLike(statement.moduleSpecifier)
  ) {
    return undefined;
  }
  const module = resolveModuleSource(statement.moduleSpecifier.text, filePath);
  if (!module || visitedFilePaths.has(module.filePath)) {
    return undefined;
  }
  return collectCallableDefinitions(
    parseSource(module.source, module.filePath),
    module.filePath,
    resolveModuleSource,
    new Set(visitedFilePaths).add(module.filePath),
  );
}

// Helper bodies that live in imported fixture modules are inspected exactly like in-file
// helpers, so an override or seed cannot hide behind a module boundary. Names defined in the
// analyzed file win over imported ones, matching how the runtime resolves them.
function collectImportedCallableDefinitions(
  sourceFile: ts.SourceFile,
  filePath: string,
  resolveModuleSource: ModuleSourceResolver | undefined,
  visitedFilePaths: Set<string>,
): Map<string, CallableDefinition> {
  const imported = new Map<string, CallableDefinition>();
  if (!resolveModuleSource) {
    return imported;
  }

  for (const statement of sourceFile.statements) {
    if (
      !ts.isImportDeclaration(statement) ||
      !statement.importClause ||
      !ts.isStringLiteralLike(statement.moduleSpecifier)
    ) {
      continue;
    }

    const module = resolveModuleSource(
      statement.moduleSpecifier.text,
      filePath,
    );
    if (!module || visitedFilePaths.has(module.filePath)) {
      continue;
    }

    const moduleDefinitions = collectCallableDefinitions(
      parseSource(module.source, module.filePath),
      module.filePath,
      resolveModuleSource,
      new Set(visitedFilePaths).add(module.filePath),
    );
    if (statement.importClause.name) {
      const defaultDefinition = moduleDefinitions.get(DEFAULT_EXPORT);
      if (defaultDefinition) {
        imported.set(
          statement.importClause.name.text,
          callableDefinitionForImport(defaultDefinition, moduleDefinitions),
        );
      }
    }
    const namedBindings = statement.importClause.namedBindings;
    if (namedBindings && ts.isNamespaceImport(namedBindings)) {
      for (const [name, definition] of moduleDefinitions) {
        if (definition.isExported && name !== DEFAULT_EXPORT) {
          imported.set(
            `${namedBindings.name.text}.${name}`,
            callableDefinitionForImport(definition, moduleDefinitions),
          );
        }
      }
      continue;
    }
    if (!namedBindings || !ts.isNamedImports(namedBindings)) {
      continue;
    }
    for (const element of namedBindings.elements) {
      const importedName = (element.propertyName ?? element.name).text;
      const definition = moduleDefinitions.get(importedName);
      if (definition?.isExported) {
        imported.set(element.name.text, {
          ...callableDefinitionForImport(definition, moduleDefinitions),
          isCanonicalNegativeControlEnvironmentCheck:
            importedName === NEGATIVE_CONTROL_ENVIRONMENT_CHECK &&
            definition.isCanonicalNegativeControlEnvironmentCheck,
        });
      }
    }
  }

  return imported;
}

interface CallableExportContext {
  filePath: string;
  resolveModuleSource: ModuleSourceResolver | undefined;
  visitedFilePaths: Set<string>;
}

function registerCallableExports(
  sourceFile: ts.SourceFile,
  definitions: Map<string, CallableDefinition>,
  context: CallableExportContext,
): void {
  for (const statement of sourceFile.statements) {
    if (
      ts.isExportAssignment(statement) &&
      ts.isIdentifier(statement.expression)
    ) {
      const definition = definitions.get(statement.expression.text);
      if (definition) {
        definitions.set(DEFAULT_EXPORT, { ...definition, isExported: true });
      }
    }
    if (!ts.isExportDeclaration(statement)) {
      continue;
    }
    const reExportedDefinitions = reExportedCallableDefinitions(
      statement,
      context.filePath,
      context.resolveModuleSource,
      context.visitedFilePaths,
    );
    if (!statement.exportClause) {
      for (const [name, definition] of reExportedDefinitions ?? []) {
        if (
          definition.isExported &&
          name !== DEFAULT_EXPORT &&
          !definitions.has(name)
        ) {
          definitions.set(name, {
            ...callableDefinitionForImport(definition, reExportedDefinitions!),
            isExported: true,
          });
        }
      }
      continue;
    }
    if (ts.isNamedExports(statement.exportClause)) {
      for (const element of statement.exportClause.elements) {
        const localName = (element.propertyName ?? element.name).text;
        const exportedName = element.name.text;
        const definition = (reExportedDefinitions ?? definitions).get(
          localName,
        );
        if (definition) {
          definitions.set(exportedName, {
            ...(reExportedDefinitions
              ? callableDefinitionForImport(definition, reExportedDefinitions)
              : definition),
            isExported: true,
          });
        }
      }
    }
  }
}

export function collectCallableDefinitions(
  sourceFile: ts.SourceFile,
  filePath: string,
  resolveModuleSource?: ModuleSourceResolver,
  visitedFilePaths: Set<string> = new Set([filePath]),
): Map<string, CallableDefinition> {
  const definitions = collectImportedCallableDefinitions(
    sourceFile,
    filePath,
    resolveModuleSource,
    visitedFilePaths,
  );

  function visit(node: ts.Node): void {
    if (ts.isFunctionDeclaration(node) && node.name) {
      const definition = {
        node,
        isCanonicalNegativeControlEnvironmentCheck:
          isCanonicalNegativeControlEnvironmentCheckDefinition(
            filePath,
            node.name.text,
          ),
        canonicalVectorFixtureOperationName:
          canonicalVectorFixtureOperationName(filePath, node.name.text),
        isExported: hasExportModifier(node),
      };
      definitions.set(node.name.text, definition);
      if (hasDefaultExportModifier(node)) {
        definitions.set(DEFAULT_EXPORT, { ...definition, isExported: true });
      }
    } else if (
      ts.isFunctionDeclaration(node) &&
      hasDefaultExportModifier(node)
    ) {
      definitions.set(DEFAULT_EXPORT, {
        node,
        isCanonicalNegativeControlEnvironmentCheck: false,
        canonicalVectorFixtureOperationName: undefined,
        isExported: true,
      });
    } else if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.initializer &&
      (ts.isArrowFunction(node.initializer) ||
        ts.isFunctionExpression(node.initializer))
    ) {
      definitions.set(node.name.text, {
        node: node.initializer,
        isCanonicalNegativeControlEnvironmentCheck:
          isCanonicalNegativeControlEnvironmentCheckDefinition(
            filePath,
            node.name.text,
          ),
        canonicalVectorFixtureOperationName:
          canonicalVectorFixtureOperationName(filePath, node.name.text),
        isExported: Boolean(
          ts.isVariableDeclarationList(node.parent) &&
          ts.isVariableStatement(node.parent.parent) &&
          hasExportModifier(node.parent.parent),
        ),
      });
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  registerObjectCallableDefinitions(sourceFile, definitions);
  registerCallableExports(sourceFile, definitions, {
    filePath,
    resolveModuleSource,
    visitedFilePaths,
  });
  return definitions;
}
