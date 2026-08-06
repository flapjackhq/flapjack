import ts from 'typescript';

/**
 * Shared AST vocabulary for the vector-capability structural analyzer: the node/definition
 * types every collector passes around, the canonical fixture-operation names, and the pure
 * reference helpers that read a call's callable name. Higher layers (definition resolution,
 * constant resolution, per-guard collectors, and the negative-control gate) all build on
 * these primitives, so they live in one dependency-free module.
 */

export type CallableNode =
  | ts.FunctionDeclaration
  | ts.FunctionExpression
  | ts.ArrowFunction
  | ts.MethodDeclaration;

export interface CallableDefinition {
  node: CallableNode;
  isCanonicalNegativeControlEnvironmentCheck: boolean;
  canonicalVectorFixtureOperationName?: string;
  isExported: boolean;
  scopeDefinitions?: Map<string, CallableDefinition>;
}

export interface ConstantDefinition {
  initializer: ts.Expression;
  isCanonicalNegativeControlTitle: boolean;
}

export interface VectorDocumentEvent {
  kind: 'seed' | 'readinessWait';
  indexIdentity: string | undefined;
  objectIds: string[];
}

export const CAPABILITY_CHECK = 'isVectorSearchEnabled';
export const EMBEDDER_SETUP = 'configureEmbedder';
export const VECTOR_SKIP_GUARD = 'skipWhenVectorSearchDisabled';
export const VECTOR_ENABLED_TEST_OPERATIONS = [
  CAPABILITY_CHECK,
  EMBEDDER_SETUP,
  'addDocumentsWithVectors',
  'setChatReadySettings',
  'waitForEmbedder',
] as const;
export const VECTOR_DOCUMENT_SEED = 'addDocumentsWithVectors';
export const VECTOR_SEARCH_READINESS_WAIT = 'waitForSearchableObjectIds';
export const CANONICAL_VECTOR_FIXTURE_OPERATIONS = [
  VECTOR_SKIP_GUARD,
  ...VECTOR_ENABLED_TEST_OPERATIONS,
  VECTOR_DOCUMENT_SEED,
  VECTOR_SEARCH_READINESS_WAIT,
];
export const NEGATIVE_CONTROL_ENVIRONMENT_CHECK = 'isP20TextOnlyNegativeControl';
export const NEGATIVE_CONTROL_ENVIRONMENT_CHECK_MODULE = 'p20_negative_control.ts';
export const NEGATIVE_CONTROL_TEST_TITLE = 'set search mode to Neural Search and verify persistence';
export const NEGATIVE_CONTROL_TEST_TITLE_IDENTIFIER = 'P20_TEXT_ONLY_CONTROL_TEST_TITLE';
export const DEFAULT_EXPORT = 'default';
export const RUNNING_TEST_TITLE_PROPERTY = 'title';
export const ANALYZED_SOURCE_FILE_NAME = 'browser-spec.ts';
// Argument positions in the `api-helpers` signatures these events are read from:
// addDocumentsWithVectors(request, indexName, documents)
// waitForSearchableObjectIds(request, indexName, query, expectedObjectIds, ...)
export const INDEX_NAME_ARGUMENT_INDEX = 1;
export const SEEDED_DOCUMENTS_ARGUMENT_INDEX = 2;
export const AWAITED_OBJECT_IDS_ARGUMENT_INDEX = 3;

export function parseSource(source: string, filePath: string): ts.SourceFile {
  return ts.createSourceFile(filePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

export function propertyName(name: ts.PropertyName): string | undefined {
  if (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNumericLiteral(name)) {
    return name.text;
  }
  return undefined;
}

export function hasDefaultExportModifier(node: ts.Node): boolean {
  return Boolean(
    ts.canHaveModifiers(node) &&
      ts
        .getModifiers(node)
        ?.some((modifier) => modifier.kind === ts.SyntaxKind.DefaultKeyword),
  );
}

export function hasExportModifier(node: ts.Node): boolean {
  return Boolean(
    ts.canHaveModifiers(node) &&
      ts
        .getModifiers(node)
        ?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword),
  );
}

export function expressionReferenceName(expression: ts.Expression): string | undefined {
  if (ts.isIdentifier(expression)) {
    return expression.text;
  }
  if (ts.isPropertyAccessExpression(expression)) {
    const receiverName = expressionReferenceName(expression.expression);
    return receiverName ? `${receiverName}.${expression.name.text}` : undefined;
  }
  return undefined;
}

export function callableReferenceName(call: ts.CallExpression): string | undefined {
  return expressionReferenceName(call.expression);
}

export function callableOperationName(callableName: string | undefined): string | undefined {
  const parts = callableName?.split('.');
  return parts?.[parts.length - 1];
}

export function matchesCallableOperation(
  callableName: string | undefined,
  operationName: string,
  definitions: Map<string, CallableDefinition>,
): boolean {
  if (!callableName) {
    return false;
  }
  return (
    callableOperationName(callableName) === operationName &&
    definitions.get(callableName)?.canonicalVectorFixtureOperationName === operationName
  );
}
