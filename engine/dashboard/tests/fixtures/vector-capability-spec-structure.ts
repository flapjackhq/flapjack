import ts from "typescript";
import type { ModuleSourceResolver } from "./spec-module-sources";
import {
  ANALYZED_SOURCE_FILE_NAME,
  AWAITED_OBJECT_IDS_ARGUMENT_INDEX,
  CAPABILITY_CHECK,
  EMBEDDER_SETUP,
  INDEX_NAME_ARGUMENT_INDEX,
  SEEDED_DOCUMENTS_ARGUMENT_INDEX,
  VECTOR_DOCUMENT_SEED,
  VECTOR_ENABLED_TEST_OPERATIONS,
  VECTOR_SEARCH_READINESS_WAIT,
  VECTOR_SKIP_GUARD,
  type CallableDefinition,
  type CallableNode,
  type ConstantDefinition,
  type VectorDocumentEvent,
  callableReferenceName,
  matchesCallableOperation,
  parseSource,
} from "./vector-capability-ast";
import {
  awaitedObjectIds,
  collectConstantDefinitions,
  constantsWithCallArguments,
  resolvedIndexIdentity,
  seededObjectIds,
} from "./vector-capability-constants";
import { collectCallableDefinitions } from "./vector-capability-definitions";
import {
  containsCompiledOutPayload,
  containsVectorEnabledHealthRouteOverride,
} from "./vector-capability-health-override";

export interface VectorCapabilityAnalysisOptions {
  /** Absolute path of the analyzed source, used to resolve its relative imports. */
  filePath?: string;
  resolveModuleSource?: ModuleSourceResolver;
}

interface VectorCapabilityStructureAnalysis {
  compiledOutTestCount: number;
  capabilityGatedCompiledOutTests: string[];
  compiledOutTestsWithoutEmbedderSetup: string[];
  vectorEnabledTestsWithoutSkipGuard: string[];
  vectorEnabledTestsWithHealthCapabilityOverride: string[];
  vectorDocumentTestsWithoutReadinessWait: string[];
}

// Vector seeds and readiness waits in source order, with helper bodies expanded at their
// call site, so a readiness wait can be attributed to the seed it actually follows.
function collectVectorDocumentEvents(
  node: ts.Node,
  definitions: Map<string, CallableDefinition>,
  constants: Map<string, ConstantDefinition>,
  visitedHelpers = new Set<string>(),
): VectorDocumentEvent[] {
  const events: VectorDocumentEvent[] = [];

  function visit(descendant: ts.Node): void {
    if (ts.isCallExpression(descendant)) {
      const name = callableReferenceName(descendant);
      if (matchesCallableOperation(name, VECTOR_DOCUMENT_SEED, definitions)) {
        events.push({
          kind: "seed",
          indexIdentity: resolvedIndexIdentity(
            descendant.arguments[INDEX_NAME_ARGUMENT_INDEX],
            constants,
          ),
          objectIds: seededObjectIds(
            descendant.arguments[SEEDED_DOCUMENTS_ARGUMENT_INDEX],
            constants,
          ),
        });
      } else if (
        matchesCallableOperation(
          name,
          VECTOR_SEARCH_READINESS_WAIT,
          definitions,
        )
      ) {
        events.push({
          kind: "readinessWait",
          indexIdentity: resolvedIndexIdentity(
            descendant.arguments[INDEX_NAME_ARGUMENT_INDEX],
            constants,
          ),
          objectIds: awaitedObjectIds(
            descendant.arguments[AWAITED_OBJECT_IDS_ARGUMENT_INDEX],
            constants,
          ),
        });
      } else {
        const definition = name ? definitions.get(name) : undefined;
        const helper = definition?.node;
        if (name && definition && helper && !visitedHelpers.has(name)) {
          events.push(
            ...collectVectorDocumentEvents(
              helper,
              definition.scopeDefinitions ?? definitions,
              constantsWithCallArguments(constants, helper, descendant),
              new Set(visitedHelpers).add(name),
            ),
          );
        }
      }
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return events;
}

// A seed is proven ready only when later waits on the same index cover every objectID
// that seed added. Any unresolved index or objectID leaves readiness unknown.
function hasSeedWithoutSubsequentReadinessWait(
  events: VectorDocumentEvent[],
): boolean {
  return events.some((event, index) => {
    if (
      event.kind !== "seed" ||
      event.indexIdentity === undefined ||
      event.objectIds.length === 0
    ) {
      return event.kind === "seed";
    }

    const laterReadyObjectIds = new Set(
      events
        .slice(index + 1)
        .filter(
          (later) =>
            later.kind === "readinessWait" &&
            later.indexIdentity === event.indexIdentity,
        )
        .flatMap((later) => later.objectIds),
    );

    return event.objectIds.some(
      (objectId) => !laterReadyObjectIds.has(objectId),
    );
  });
}

function invokes(
  node: ts.Node,
  targetName: string,
  definitions: Map<string, CallableDefinition>,
  visitedHelpers = new Set<string>(),
): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (ts.isCallExpression(descendant)) {
      const name = callableReferenceName(descendant);
      if (matchesCallableOperation(name, targetName, definitions)) {
        found = true;
        return;
      }
      const definition = name ? definitions.get(name) : undefined;
      const helper = definition?.node;
      if (name && definition && helper && !visitedHelpers.has(name)) {
        const nextVisitedHelpers = new Set(visitedHelpers).add(name);
        if (
          invokes(
            helper,
            targetName,
            definition.scopeDefinitions ?? definitions,
            nextVisitedHelpers,
          )
        ) {
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
  definitions: Map<string, CallableDefinition>,
): boolean {
  return targetNames.some((targetName) =>
    invokes(node, targetName, definitions),
  );
}

function playwrightCallKind(call: ts.CallExpression): string | undefined {
  if (ts.isIdentifier(call.expression) && call.expression.text === "test") {
    return "test";
  }
  if (
    ts.isPropertyAccessExpression(call.expression) &&
    ts.isIdentifier(call.expression.expression) &&
    call.expression.expression.text === "test"
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
  return title && ts.isStringLiteralLike(title) ? title.text : "<unnamed test>";
}

function isVectorEnabledDescribe(call: ts.CallExpression): boolean {
  if (playwrightCallKind(call) !== "describe") {
    return false;
  }
  return testTitle(call).toLowerCase().includes("vector-enabled");
}

function expressionCall(
  statement: ts.Statement,
): ts.CallExpression | undefined {
  return ts.isExpressionStatement(statement) &&
    ts.isCallExpression(statement.expression)
    ? statement.expression
    : undefined;
}

function callableBodyCall(
  callable: CallableNode,
): ts.CallExpression | undefined {
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
  definitions: Map<string, CallableDefinition>,
  visitedHelpers = new Set<string>(),
): ts.CallExpression[] {
  return container.statements.flatMap((statement) => {
    const call = expressionCall(statement);
    return call
      ? registeredPlaywrightCallsFrom(call, definitions, visitedHelpers)
      : [];
  });
}

function registeredPlaywrightCallsFrom(
  call: ts.CallExpression,
  definitions: Map<string, CallableDefinition>,
  visitedHelpers: Set<string>,
): ts.CallExpression[] {
  if (playwrightCallKind(call)) {
    return [call];
  }

  const name = callableReferenceName(call);
  const definition = name ? definitions.get(name) : undefined;
  const helper = definition?.node;
  if (!name || !definition || !helper || visitedHelpers.has(name)) {
    return [];
  }

  const nextVisitedHelpers = new Set(visitedHelpers).add(name);
  const helperDefinitions = definition.scopeDefinitions ?? definitions;
  if (helper.body && ts.isBlock(helper.body)) {
    return collectRegisteredPlaywrightCalls(
      helper.body,
      helperDefinitions,
      nextVisitedHelpers,
    );
  }
  const bodyCall = callableBodyCall(helper);
  return bodyCall
    ? registeredPlaywrightCallsFrom(
        bodyCall,
        helperDefinitions,
        nextVisitedHelpers,
      )
    : [];
}

interface AnalyzerContext {
  definitions: Map<string, CallableDefinition>;
  constants: Map<string, ConstantDefinition>;
  analysis: VectorCapabilityStructureAnalysis;
}

interface ContainerScope {
  inheritedSetups: CallableNode[];
  inheritedVectorEnabledDescribe: boolean;
}

interface TestInspectionScope {
  callback: CallableNode;
  applicableSetups: CallableNode[];
  inheritedVectorEnabledDescribe: boolean;
  title: string;
}

function inspectVectorEnabledTest(
  scope: TestInspectionScope,
  context: AnalyzerContext,
): void {
  const { definitions, constants, analysis } = context;
  const callableScope = [...scope.applicableSetups, scope.callback];
  const hasSkipGuard = callableScope.some((setup) =>
    invokes(setup, VECTOR_SKIP_GUARD, definitions),
  );
  const requiresVectorEnabled =
    scope.inheritedVectorEnabledDescribe ||
    callableScope.some((setup) =>
      invokes(setup, CAPABILITY_CHECK, definitions),
    ) ||
    invokesAny(scope.callback, VECTOR_ENABLED_TEST_OPERATIONS, definitions);
  if (requiresVectorEnabled && !hasSkipGuard) {
    analysis.vectorEnabledTestsWithoutSkipGuard.push(scope.title);
  }
  if (
    requiresVectorEnabled &&
    callableScope.some((setup) =>
      containsVectorEnabledHealthRouteOverride(setup, definitions, constants),
    )
  ) {
    analysis.vectorEnabledTestsWithHealthCapabilityOverride.push(scope.title);
  }
  const vectorDocumentEvents = callableScope.flatMap((setup) =>
    collectVectorDocumentEvents(setup, definitions, constants),
  );
  if (hasSeedWithoutSubsequentReadinessWait(vectorDocumentEvents)) {
    analysis.vectorDocumentTestsWithoutReadinessWait.push(scope.title);
  }
}

function inspectCompiledOutTest(
  scope: TestInspectionScope,
  context: AnalyzerContext,
): void {
  const { definitions, constants, analysis } = context;
  if (!containsCompiledOutPayload(scope.callback, definitions, constants)) {
    return;
  }

  analysis.compiledOutTestCount += 1;
  if (
    scope.applicableSetups.some((setup) =>
      invokes(setup, CAPABILITY_CHECK, definitions),
    )
  ) {
    analysis.capabilityGatedCompiledOutTests.push(scope.title);
  }
  const establishesEmbedder = [...scope.applicableSetups, scope.callback].some(
    (setup) => invokes(setup, EMBEDDER_SETUP, definitions),
  );
  if (!establishesEmbedder) {
    analysis.compiledOutTestsWithoutEmbedderSetup.push(scope.title);
  }
}

function inspectContainer(
  container: ts.SourceFile | ts.Block,
  scope: ContainerScope,
  context: AnalyzerContext,
): void {
  const calls = collectRegisteredPlaywrightCalls(
    container,
    context.definitions,
  );
  const localSetups = calls
    .filter((call) =>
      ["beforeAll", "beforeEach"].includes(playwrightCallKind(call) ?? ""),
    )
    .map(callbackArgument)
    .filter((callback): callback is CallableNode => callback !== undefined);
  const applicableSetups = [...scope.inheritedSetups, ...localSetups];

  for (const call of calls) {
    const kind = playwrightCallKind(call);
    const callback = callbackArgument(call);
    if (kind === "describe" && callback?.body && ts.isBlock(callback.body)) {
      inspectContainer(
        callback.body,
        {
          inheritedSetups: applicableSetups,
          inheritedVectorEnabledDescribe:
            scope.inheritedVectorEnabledDescribe ||
            isVectorEnabledDescribe(call),
        },
        context,
      );
      continue;
    }
    if (kind === "test" && callback) {
      const testScope = {
        callback,
        applicableSetups,
        inheritedVectorEnabledDescribe: scope.inheritedVectorEnabledDescribe,
        title: testTitle(call),
      };
      inspectVectorEnabledTest(testScope, context);
      inspectCompiledOutTest(testScope, context);
    }
  }
}

export function analyzeVectorCapabilityStructure(
  source: string,
  options: VectorCapabilityAnalysisOptions = {},
): VectorCapabilityStructureAnalysis {
  const filePath = options.filePath ?? ANALYZED_SOURCE_FILE_NAME;
  const sourceFile = parseSource(source, filePath);
  const analysis = {
    compiledOutTestCount: 0,
    capabilityGatedCompiledOutTests: [],
    compiledOutTestsWithoutEmbedderSetup: [],
    vectorEnabledTestsWithoutSkipGuard: [],
    vectorEnabledTestsWithHealthCapabilityOverride: [],
    vectorDocumentTestsWithoutReadinessWait: [],
  } satisfies VectorCapabilityStructureAnalysis;
  const context = {
    definitions: collectCallableDefinitions(
      sourceFile,
      filePath,
      options.resolveModuleSource,
    ),
    constants: collectConstantDefinitions(
      sourceFile,
      filePath,
      options.resolveModuleSource,
    ),
    analysis,
  };

  inspectContainer(
    sourceFile,
    { inheritedSetups: [], inheritedVectorEnabledDescribe: false },
    context,
  );
  return analysis;
}
