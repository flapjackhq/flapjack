import ts from 'typescript';
import type { ModuleSourceResolver } from './spec-module-sources';
import {
  NEGATIVE_CONTROL_ENVIRONMENT_CHECK_MODULE,
  NEGATIVE_CONTROL_TEST_TITLE_IDENTIFIER,
  type CallableNode,
  type ConstantDefinition,
  parseSource,
  propertyName,
} from './vector-capability-ast';

/**
 * Resolves `const` bindings — including ones pulled in through named imports — and follows
 * identifier references to their declared initializer. Seeded documents, awaited objectIDs,
 * and index names written as shared constants therefore compare by value, not by spelling,
 * which is what lets the readiness guard match a seed to the wait that actually covers it.
 */

function isCanonicalNegativeControlTitleDefinition(filePath: string, name: string): boolean {
  return (
    name === NEGATIVE_CONTROL_TEST_TITLE_IDENTIFIER &&
    filePath.replace(/\\/g, '/').endsWith(`/${NEGATIVE_CONTROL_ENVIRONMENT_CHECK_MODULE}`)
  );
}

function collectImportedConstantDefinitions(
  sourceFile: ts.SourceFile,
  filePath: string,
  resolveModuleSource: ModuleSourceResolver | undefined,
  visitedFilePaths: Set<string>,
): Map<string, ConstantDefinition> {
  const imported = new Map<string, ConstantDefinition>();
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

    const namedBindings = statement.importClause.namedBindings;
    if (!namedBindings || !ts.isNamedImports(namedBindings)) {
      continue;
    }

    const module = resolveModuleSource(statement.moduleSpecifier.text, filePath);
    if (!module || visitedFilePaths.has(module.filePath)) {
      continue;
    }

    const moduleConstants = collectConstantDefinitions(
      parseSource(module.source, module.filePath),
      module.filePath,
      resolveModuleSource,
      new Set(visitedFilePaths).add(module.filePath),
    );
    for (const element of namedBindings.elements) {
      const importedName = (element.propertyName ?? element.name).text;
      const definition = moduleConstants.get(importedName);
      if (definition) {
        imported.set(element.name.text, definition);
      }
    }
  }

  return imported;
}

export function collectConstantDefinitions(
  sourceFile: ts.SourceFile,
  filePath: string,
  resolveModuleSource?: ModuleSourceResolver,
  visitedFilePaths: Set<string> = new Set([filePath]),
): Map<string, ConstantDefinition> {
  const constants = collectImportedConstantDefinitions(
    sourceFile,
    filePath,
    resolveModuleSource,
    visitedFilePaths,
  );

  function visit(node: ts.Node): void {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.initializer &&
      ts.isVariableDeclarationList(node.parent) &&
      (node.parent.flags & ts.NodeFlags.Const) !== 0 &&
      !ts.isArrowFunction(node.initializer) &&
      !ts.isFunctionExpression(node.initializer)
    ) {
      constants.set(node.name.text, {
        initializer: node.initializer,
        isCanonicalNegativeControlTitle: isCanonicalNegativeControlTitleDefinition(
          filePath,
          node.name.text,
        ),
      });
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  return constants;
}

// Follows identifier references to their declared initializer so proof documents and
// objectIDs written as shared constants compare by value, not by spelling.
export function resolveExpression(
  expression: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
  visitedConstants = new Set<string>(),
): ts.Expression | undefined {
  if (!expression || !ts.isIdentifier(expression)) {
    return expression;
  }
  const name = expression.text;
  const definition = constants.get(name);
  if (!definition) {
    return expression;
  }
  if (visitedConstants.has(name)) {
    return undefined;
  }
  return resolveExpression(
    definition.initializer,
    constants,
    new Set(visitedConstants).add(name),
  );
}

function resolvedStringValue(
  expression: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
): string | undefined {
  const resolved = resolveExpression(expression, constants);
  return resolved && ts.isStringLiteralLike(resolved) ? resolved.text : undefined;
}

function resolvedArrayElements(
  expression: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
): readonly ts.Expression[] {
  const resolved = resolveExpression(expression, constants);
  return resolved && ts.isArrayLiteralExpression(resolved) ? resolved.elements : [];
}

export function resolvedIndexIdentity(
  expression: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
): string | undefined {
  const resolved = resolveExpression(expression, constants);
  if (resolved && ts.isStringLiteralLike(resolved)) {
    return `value:${resolved.text}`;
  }
  if (resolved && ts.isIdentifier(resolved)) {
    return `identifier:${resolved.text}`;
  }
  return undefined;
}

export function seededObjectIds(
  documentsArgument: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
): string[] {
  return resolvedArrayElements(documentsArgument, constants).flatMap((element) => {
    const document = resolveExpression(element, constants);
    if (!document || !ts.isObjectLiteralExpression(document)) {
      return [];
    }
    const objectId = document.properties.find(
      (property): property is ts.PropertyAssignment =>
        ts.isPropertyAssignment(property) && propertyName(property.name) === 'objectID',
    );
    const value = objectId && resolvedStringValue(objectId.initializer, constants);
    return value ? [value] : [];
  });
}

export function awaitedObjectIds(
  expectedObjectIdsArgument: ts.Expression | undefined,
  constants: Map<string, ConstantDefinition>,
): string[] {
  return resolvedArrayElements(expectedObjectIdsArgument, constants).flatMap((element) => {
    const value = resolvedStringValue(element, constants);
    return value ? [value] : [];
  });
}

export function constantsWithCallArguments(
  constants: Map<string, ConstantDefinition>,
  helper: CallableNode,
  call: ts.CallExpression,
): Map<string, ConstantDefinition> {
  const callConstants = new Map(constants);

  helper.parameters.forEach((parameter, index) => {
    if (!ts.isIdentifier(parameter.name)) {
      return;
    }
    const argument = call.arguments[index] ?? parameter.initializer;
    if (argument) {
      callConstants.set(
        parameter.name.text,
        {
          initializer: resolveExpression(argument, constants) ?? argument,
          isCanonicalNegativeControlTitle:
            ts.isIdentifier(argument) &&
            constants.get(argument.text)?.isCanonicalNegativeControlTitle === true,
        },
      );
    }
  });

  return callConstants;
}
