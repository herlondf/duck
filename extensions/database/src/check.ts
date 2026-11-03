// npm test: arquivo de consultas, tabela e comandos Redis.
import * as assert from "node:assert";
import { parseQueries, formatQuery, table, tokenize, redisCommands } from "./lib";

const qs = parseQueries("-- cabecalho\n-- name: Um\nselect 1;\n\n-- name: Dois\r\nselect\n  2;\n-- name: Vazia\n");
assert.deepStrictEqual(qs, [{ name: "Um", text: "select 1;" }, { name: "Dois", text: "select\n  2;" }]);
assert.deepStrictEqual(parseQueries(formatQuery("Três", "DBSIZE")), [{ name: "Três", text: "DBSIZE" }]);

const t = table(["id", "nome"], [[1, "a".repeat(60)], [22, null]], 5).split("\n");
assert.strictEqual(t[0], "id │ nome");
assert.strictEqual(t[2], `1  │ ${"a".repeat(39)}…`);
assert.strictEqual(t[3], "22 │ NULL");
assert.strictEqual(t[t.length - 1], "2 linhas em 5 ms");
const big = table(["n"], Array.from({ length: 250 }, (_, i) => [i]), 1);
assert.ok(big.includes("mostrando 200 de 250") && big.endsWith("250 linhas em 1 ms"));
assert.strictEqual(big.split("\n").length, 2 + 200 + 1 + 2);

assert.deepStrictEqual(tokenize(`SET "a b" 'c d' e\\"f`), ["SET", "a b", "c d", 'e\\"f']);
assert.deepStrictEqual(redisCommands("-- x\nDBSIZE\n\nGET k\n"), [["DBSIZE"], ["GET", "k"]]);
console.log("ok");
