import ts from 'typescript';
import {
  NEGATIVE_CONTROL_TEST_TITLE,
  RUNNING_TEST_TITLE_PROPERTY,
  type CallableDefinition,
  type ConstantDefinition,
  callableReferenceName,
  propertyName,
} from './vector-capability-ast';
import { resolveExpression } from './vector-capability-constants';

/**
 * Detects browser `/health` capability overrides — a `route('**\/health', ...)` whose
 * fulfillment payload forces `vectorSearch` to a fixed value — and recognizes the one
 * legitimate exception: the committed text-only negative control, which only stops being
 * positive-path coverage when its gate proves both that the control environment is active
 * and that it applies to the single control test.
 */

function setsVectorSearchValue(node: ts.Node, valueKind: ts.SyntaxKind): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (
      ts.isPropertyAssignment(descendant) &&
      propertyName(descendant.name) === 'vectorSearch' &&
      descendant.initializer.kind === valueKind
    ) {
      found = true;
      return;
    }
    ts.forEachChild(descendant, visit);
  }

  visit(node);
  return found;
}

function fulfillsVectorSearchPayload(node: ts.Node, valueKind: ts.SyntaxKind): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (
      ts.isCallExpression(descendant) &&
      ts.isPropertyAccessExpression(descendant.expression) &&
      descendant.expression.name.text === 'fulfill' &&
      descendant.arguments.some((argument) => setsVectorSearchValue(argument, valueKind))
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

function resolvedConditionOperand(
  expression: ts.Expression,
  constants: Map<string, ConstantDefinition>,
): ts.Expression | undefined {
  const resolved = resolveExpression(expression, constants);
  if (!resolved) {
    return undefined;
  }
  if (ts.isParenthesizedExpression(resolved)) {
    return resolvedConditionOperand(resolved.expression, constants);
  }
  return resolved;
}

function positiveConjuncts(
  expression: ts.Expression,
  constants: Map<string, ConstantDefinition>,
): ts.Expression[] {
  const resolved = resolvedConditionOperand(expression, constants);
  if (
    resolved &&
    ts.isBinaryExpression(resolved) &&
    resolved.operatorToken.kind === ts.SyntaxKind.AmpersandAmpersandToken
  ) {
    return [
      ...positiveConjuncts(resolved.left, constants),
      ...positiveConjuncts(resolved.right, constants),
    ];
  }
  return resolved ? [resolved] : [];
}

function isNegativeControlEnvironmentCheck(
  expression: ts.Expression,
  constants: Map<string, ConstantDefinition>,
  definitions: Map<string, CallableDefinition>,
): boolean {
  const resolved = resolvedConditionOperand(expression, constants);
  if (!resolved || !ts.isCallExpression(resolved) || !ts.isIdentifier(resolved.expression)) {
    return false;
  }
  return (
    definitions.get(resolved.expression.text)?.isCanonicalNegativeControlEnvironmentCheck ===
    true
  );
}

function isRunningTestTitleAccess(expression: ts.Expression): boolean {
  return (
    ts.isPropertyAccessExpression(expression) &&
    ts.isIdentifier(expression.expression) &&
    expression.expression.text === 'testInfo' &&
    expression.name.text === RUNNING_TEST_TITLE_PROPERTY
  );
}

function isNegativeControlTitleReference(
  expression: ts.Expression,
  constants: Map<string, ConstantDefinition>,
): boolean {
  const directReferenceIsCanonical =
    ts.isIdentifier(expression) &&
    constants.get(expression.text)?.isCanonicalNegativeControlTitle === true;
  const resolved = resolvedConditionOperand(expression, constants);
  if (!resolved) {
    return false;
  }
  if (ts.isIdentifier(resolved)) {
    return (
      directReferenceIsCanonical ||
      constants.get(resolved.text)?.isCanonicalNegativeControlTitle === true
    );
  }
  return ts.isStringLiteralLike(resolved) && resolved.text === NEGATIVE_CONTROL_TEST_TITLE;
}

function isNegativeControlTitleEquality(
  expression: ts.Expression,
  constants: Map<string, ConstantDefinition>,
): boolean {
  const resolved = resolvedConditionOperand(expression, constants);
  if (
    !resolved ||
    !ts.isBinaryExpression(resolved) ||
    resolved.operatorToken.kind !== ts.SyntaxKind.EqualsEqualsEqualsToken
  ) {
    return false;
  }

  return (
    (isRunningTestTitleAccess(resolved.left) &&
      isNegativeControlTitleReference(resolved.right, constants)) ||
    (isRunningTestTitleAccess(resolved.right) &&
      isNegativeControlTitleReference(resolved.left, constants))
  );
}

// The committed text-only negative control installs a deliberate vector-enabled `/health`
// override. It stops being positive-path coverage only when its gate proves both that the
// control environment is active and that it applies to the single control test, so an
// override that could run in an ordinary suite, or leak to sibling tests in the same
// describe, is still reported.
function isNegativeControlTestGate(
  condition: ts.Expression,
  constants: Map<string, ConstantDefinition>,
  definitions: Map<string, CallableDefinition>,
): boolean {
  const conjuncts = positiveConjuncts(condition, constants);
  return (
    conjuncts.some((conjunct) =>
      isNegativeControlEnvironmentCheck(conjunct, constants, definitions),
    ) &&
    conjuncts.some((conjunct) => isNegativeControlTitleEquality(conjunct, constants))
  );
}

// A `/health` capability override only exists when a browser test installs an executable
// route override whose fulfillment payload forces `vectorSearch` to `capabilityValue`. An
// ordinary expected-value object that merely spells `vectorSearch: false` (e.g. in a
// `toEqual` assertion) is not compiled-out coverage. Helper-mediated route setup is
// followed the same way capability-setup detection follows helpers.
function containsHealthCapabilityOverride(
  node: ts.Node,
  capabilityValue: ts.SyntaxKind,
  definitions: Map<string, CallableDefinition>,
  constants: Map<string, ConstantDefinition>,
  visitedHelpers = new Set<string>(),
): boolean {
  let found = false;

  function visit(descendant: ts.Node): void {
    if (found) {
      return;
    }
    if (
      ts.isIfStatement(descendant) &&
      isNegativeControlTestGate(descendant.expression, constants, definitions)
    ) {
      if (descendant.elseStatement) {
        visit(descendant.elseStatement);
      }
      return;
    }
    if (ts.isCallExpression(descendant)) {
      if (
        isHealthRouteOverride(descendant) &&
        fulfillsVectorSearchPayload(descendant, capabilityValue)
      ) {
        found = true;
        return;
      }
      const name = callableReferenceName(descendant);
      const definition = name ? definitions.get(name) : undefined;
      const helper = definition?.node;
      if (name && definition && helper && !visitedHelpers.has(name)) {
        const nextVisitedHelpers = new Set(visitedHelpers).add(name);
        if (
          containsHealthCapabilityOverride(
            helper,
            capabilityValue,
            definition.scopeDefinitions ?? definitions,
            constants,
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

export function containsCompiledOutPayload(
  node: ts.Node,
  definitions: Map<string, CallableDefinition>,
  constants: Map<string, ConstantDefinition>,
): boolean {
  return containsHealthCapabilityOverride(
    node,
    ts.SyntaxKind.FalseKeyword,
    definitions,
    constants,
  );
}

export function containsVectorEnabledHealthRouteOverride(
  node: ts.Node,
  definitions: Map<string, CallableDefinition>,
  constants: Map<string, ConstantDefinition>,
): boolean {
  return containsHealthCapabilityOverride(
    node,
    ts.SyntaxKind.TrueKeyword,
    definitions,
    constants,
  );
}
