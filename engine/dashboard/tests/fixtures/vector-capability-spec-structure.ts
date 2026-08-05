import ts from 'typescript';

interface VectorCapabilityStructureAnalysis {
  compiledOutTestCount: number;
  capabilityGatedCompiledOutTests: string[];
  compiledOutTestsWithoutEmbedderSetup: string[];
  vectorEnabledTestsWithoutSkipGuard: string[];
}

type CallableNode = ts.FunctionDeclaration | ts.FunctionExpression | ts.ArrowFunction;

const CAPABILITY_CHECK = 'isVectorSearchEnabled';
const EMBEDDER_SETUP = 'configureEmbedder';
const VECTOR_SKIP_GUARD = 'skipWhenVectorSearchDisabled';
const VECTOR_ENABLED_TEST_OPERATIONS = [
  CAPABILITY_CHECK,
  EMBEDDER_SETUP,
  'addDocumentsWithVectors',
  'setChatReadySettings',
  'waitForEmbedder',
] as const;

function collectCallableDefinitions(sourceFile: ts.SourceFile): Map<string, CallableNode> {
  const definitions = new Map<string, CallableNode>();

  function visit(node: ts.Node): void {
    if (ts.isFunctionDeclaration(node) && node.name) {
      definitions.set(node.name.text, node);
    } else if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.initializer &&
      (ts.isArrowFunction(node.initializer) || ts.isFunctionExpression(node.initializer))
    ) {
      definitions.set(node.name.text, node.initializer);
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  return definitions;
}

function calledName(call: ts.CallExpression): string | undefined {
  if (ts.isIdentifier(call.expression)) {
    return call.expression.text;
  }
  if (ts.isPropertyAccessExpression(call.expression)) {
    return call.expression.name.text;
  }
  return undefined;
}

function invokes(
  node: ts.Node,
  targetName: string,
  definitions: Map<string, CallableNode>,
  visitedHelpers = new Set<string>(),
): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (ts.isCallExpression(descendant)) {
      const name = calledName(descendant);
      if (name === targetName) {
        found = true;
        return;
      }
      const helper = name && definitions.get(name);
      if (name && helper && !visitedHelpers.has(name)) {
        const nextVisitedHelpers = new Set(visitedHelpers).add(name);
        if (invokes(helper, targetName, definitions, nextVisitedHelpers)) {
          found = true;
          return;
        }
      }
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return found;
}

function invokesAny(
  node: ts.Node,
  targetNames: readonly string[],
  definitions: Map<string, CallableNode>,
): boolean {
  return targetNames.some((targetName) => invokes(node, targetName, definitions));
}

function setsVectorSearchFalse(node: ts.Node): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (
      ts.isPropertyAssignment(descendant) &&
      propertyName(descendant.name) === 'vectorSearch' &&
      descendant.initializer.kind === ts.SyntaxKind.FalseKeyword
    ) {
      found = true;
      return;
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return found;
}

function isHealthRouteOverride(call: ts.CallExpression): boolean {
  if (
    !ts.isPropertyAccessExpression(call.expression) ||
    call.expression.name.text !== 'route'
  ) {
    return false;
  }
  const pattern = call.arguments[0];
  return Boolean(pattern && ts.isStringLiteralLike(pattern) && pattern.text.includes('health'));
}

function fulfillsCompiledOutPayload(node: ts.Node): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (
      ts.isCallExpression(descendant) &&
      ts.isPropertyAccessExpression(descendant.expression) &&
      descendant.expression.name.text === 'fulfill' &&
      descendant.arguments.some((argument) => setsVectorSearchFalse(argument))
    ) {
      found = true;
      return;
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return found;
}

// A compiled-out browser test only exists when it installs an executable `/health`
// route override whose fulfillment payload forces `vectorSearch: false`. An ordinary
// expected-value object that merely spells `vectorSearch: false` (e.g. in a `toEqual`
// assertion) is not compiled-out coverage. Helper-mediated route setup is followed the
// same way capability-setup detection follows helpers.
function containsCompiledOutPayload(
  node: ts.Node,
  definitions: Map<string, CallableNode>,
  visitedHelpers = new Set<string>(),
): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (ts.isCallExpression(descendant)) {
      if (isHealthRouteOverride(descendant) && fulfillsCompiledOutPayload(descendant)) {
        found = true;
        return;
      }
      const name = calledName(descendant);
      const helper = name && definitions.get(name);
      if (name && helper && !visitedHelpers.has(name)) {
        const nextVisitedHelpers = new Set(visitedHelpers).add(name);
        if (containsCompiledOutPayload(helper, definitions, nextVisitedHelpers)) {
          found = true;
          return;
        }
      }
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return found;
}

function propertyName(name: ts.PropertyName): string | undefined {
  if (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNumericLiteral(name)) {
    return name.text;
  }
  return undefined;
}

function playwrightCallKind(call: ts.CallExpression): string | undefined {
  if (ts.isIdentifier(call.expression) && call.expression.text === 'test') {
    return 'test';
  }
  if (
    ts.isPropertyAccessExpression(call.expression) &&
    ts.isIdentifier(call.expression.expression) &&
    call.expression.expression.text === 'test'
  ) {
    return call.expression.name.text;
  }
  return undefined;
}

function callbackArgument(call: ts.CallExpression): CallableNode | undefined {
  for (let index = call.arguments.length - 1; index >= 0; index -= 1) {
    const argument = call.arguments[index];
    if (ts.isArrowFunction(argument) || ts.isFunctionExpression(argument)) {
      return argument;
    }
  }
  return undefined;
}

function testTitle(call: ts.CallExpression): string {
  const title = call.arguments[0];
  return title && ts.isStringLiteralLike(title) ? title.text : '<unnamed test>';
}

function isVectorEnabledDescribe(call: ts.CallExpression): boolean {
  if (playwrightCallKind(call) !== 'describe') {
    return false;
  }
  return testTitle(call).toLowerCase().includes('vector-enabled');
}

function expressionCall(statement: ts.Statement): ts.CallExpression | undefined {
  return ts.isExpressionStatement(statement) && ts.isCallExpression(statement.expression)
    ? statement.expression
    : undefined;
}

function callableBodyCall(callable: CallableNode): ts.CallExpression | undefined {
  if (!ts.isArrowFunction(callable) || ts.isBlock(callable.body)) {
    return undefined;
  }
  let expression = callable.body;
  while (ts.isParenthesizedExpression(expression)) {
    expression = expression.expression;
  }
  return ts.isCallExpression(expression) ? expression : undefined;
}

function collectRegisteredPlaywrightCalls(
  container: ts.SourceFile | ts.Block,
  definitions: Map<string, CallableNode>,
  visitedHelpers = new Set<string>(),
): ts.CallExpression[] {
  return container.statements.flatMap((statement) => {
    const call = expressionCall(statement);
    return call ? registeredPlaywrightCallsFrom(call, definitions, visitedHelpers) : [];
  });
}

function registeredPlaywrightCallsFrom(
  call: ts.CallExpression,
  definitions: Map<string, CallableNode>,
  visitedHelpers: Set<string>,
): ts.CallExpression[] {
  if (playwrightCallKind(call)) {
    return [call];
  }

  const name = calledName(call);
  const helper = name && definitions.get(name);
  if (!name || !helper || visitedHelpers.has(name)) {
    return [];
  }

  const nextVisitedHelpers = new Set(visitedHelpers).add(name);
  if (helper.body && ts.isBlock(helper.body)) {
    return collectRegisteredPlaywrightCalls(helper.body, definitions, nextVisitedHelpers);
  }
  const bodyCall = callableBodyCall(helper);
  return bodyCall
    ? registeredPlaywrightCallsFrom(bodyCall, definitions, nextVisitedHelpers)
    : [];
}

export function analyzeVectorCapabilityStructure(
  source: string,
): VectorCapabilityStructureAnalysis {
  const sourceFile = ts.createSourceFile(
    'browser-spec.ts',
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  const definitions = collectCallableDefinitions(sourceFile);
  const capabilityGatedCompiledOutTests: string[] = [];
  const compiledOutTestsWithoutEmbedderSetup: string[] = [];
  const vectorEnabledTestsWithoutSkipGuard: string[] = [];
  let compiledOutTestCount = 0;

  function inspectContainer(
    container: ts.SourceFile | ts.Block,
    inheritedSetups: CallableNode[],
    inheritedVectorEnabledDescribe: boolean,
  ): void {
    const calls = collectRegisteredPlaywrightCalls(container, definitions);
    const localSetups = calls
      .filter((call) => ['beforeAll', 'beforeEach'].includes(playwrightCallKind(call) ?? ''))
      .map(callbackArgument)
      .filter((callback): callback is CallableNode => callback !== undefined);
    const applicableSetups = [...inheritedSetups, ...localSetups];

    for (const call of calls) {
      const kind = playwrightCallKind(call);
      const callback = callbackArgument(call);
      if (kind === 'describe' && callback?.body && ts.isBlock(callback.body)) {
        inspectContainer(
          callback.body,
          applicableSetups,
          inheritedVectorEnabledDescribe || isVectorEnabledDescribe(call),
        );
        continue;
      }
      if (kind === 'test' && callback) {
        const callableScope = [...applicableSetups, callback];
        const hasSkipGuard = callableScope.some((setup) =>
          invokes(setup, VECTOR_SKIP_GUARD, definitions),
        );
        const requiresVectorEnabled =
          inheritedVectorEnabledDescribe ||
          callableScope.some((setup) => invokes(setup, CAPABILITY_CHECK, definitions)) ||
          invokesAny(callback, VECTOR_ENABLED_TEST_OPERATIONS, definitions);
        if (requiresVectorEnabled && !hasSkipGuard) {
          vectorEnabledTestsWithoutSkipGuard.push(testTitle(call));
        }
      }
      if (
        kind !== 'test' ||
        !callback ||
        !containsCompiledOutPayload(callback, definitions)
      ) {
        continue;
      }

      compiledOutTestCount += 1;
      const title = testTitle(call);
      if (applicableSetups.some((setup) => invokes(setup, CAPABILITY_CHECK, definitions))) {
        capabilityGatedCompiledOutTests.push(title);
      }
      const establishesEmbedder = [...applicableSetups, callback].some((setup) =>
        invokes(setup, EMBEDDER_SETUP, definitions),
      );
      if (!establishesEmbedder) {
        compiledOutTestsWithoutEmbedderSetup.push(title);
      }
    }
  }

  inspectContainer(sourceFile, [], false);
  return {
    compiledOutTestCount,
    capabilityGatedCompiledOutTests,
    compiledOutTestsWithoutEmbedderSetup,
    vectorEnabledTestsWithoutSkipGuard,
  };
}
