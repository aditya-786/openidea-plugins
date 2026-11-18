<?php
// Explicit opt-in runtime inspection. No file installation, shell, job execution or payload reads.
/** The only database-mode errors whose message reaches the UI; it is written by this script. */
final class OpenIdeaLaravelProbeMessage extends RuntimeException {}
$mode = $argv[1] ?? '';
$options = json_decode($argv[2] ?? '{}', true, 16, JSON_THROW_ON_ERROR);
if ($mode === 'logs') {
    $root = realpath(getcwd());
    $relative = $options['file'] ?? '';
    if (!is_string($relative) || strlen($relative) > 1024 || !str_starts_with($relative, 'storage/logs/') || !str_ends_with($relative, '.log') || preg_match('~(?:^|/)\.\.?(?:/|$)|[\x00-\x1f]~', $relative)) {
        throw new RuntimeException('Select one project storage/logs/*.log file.');
    }
    $path = realpath($root.'/'.$relative);
    $directory = realpath($root.'/storage/logs');
    if ($path === false || $directory === false || !str_starts_with($path, $directory.'/') || !str_starts_with($path, $root.'/')) {
        throw new RuntimeException('Log path escapes the project log directory.');
    }
    $stream = fopen($path, 'rb');
    if ($stream === false) { throw new RuntimeException('Selected log is unavailable.'); }
    $size = fstat($stream)['size'];
    $offset = max(0, $size - 65536);
    fseek($stream, $offset);
    $tail = fread($stream, 65536);
    fclose($stream);
    if ($tail === false) { throw new RuntimeException('Selected log is unreadable.'); }
    if ($offset > 0) { $newline = strpos($tail, "\n"); $tail = $newline === false ? '' : substr($tail, $newline + 1); }
    $lines = explode("\n", rtrim($tail, "\r\n"));
    if ($tail === '') { $lines = []; }
    echo json_encode(['file' => $relative, 'truncated' => $offset > 0 || count($lines) > 200, 'lines' => array_slice($lines, -200)], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}
require getcwd().'/vendor/autoload.php';
$app = require getcwd().'/bootstrap/app.php';
$app->make(\Illuminate\Contracts\Console\Kernel::class)->bootstrap();
if ($mode === 'model-connections') {
    $models = $options['models'] ?? null;
    if (!is_array($models) || count($models) < 1 || count($models) > 32) { throw new RuntimeException('Select between 1 and 32 owned model classes.'); }
    $root = realpath(getcwd());
    $default = $app['db']->getDefaultConnection();
    if (!is_string($default) || $default === '' || strlen($default) > 128) { throw new RuntimeException('Framework default connection is unavailable.'); }
    $result = [];
    foreach ($models as $input) {
        $class = $input['class'] ?? null;
        $file = $input['file'] ?? null;
        if (!is_string($class) || strlen($class) > 512 || !preg_match('/^[A-Za-z_][A-Za-z_0-9]*(?:\\\\[A-Za-z_][A-Za-z_0-9]*)*$/D', $class) || !is_string($file) || strlen($file) > 1024 || str_contains($file, chr(92)) || preg_match('~(?:^|/)\.\.?(?:/|$)|[\x00-\x1f]~', $file) || str_starts_with($file, '/')) { throw new RuntimeException('Invalid owned model request.'); }
        $row = ['class' => $class, 'file' => $file, 'available' => false, 'dependencies' => []];
        try {
            $expected = realpath($root.'/'.$file);
            $reflection = new ReflectionClass($class);
            $actual = $reflection->getFileName();
            if ($expected === false || !str_starts_with($expected, $root.'/') || !is_string($actual) || realpath($actual) !== $expected || $reflection->isAbstract() || !$reflection->isSubclassOf(\Illuminate\Database\Eloquent\Model::class) || $reflection->getName() !== $class) { throw new RuntimeException('Unproven model declaration.'); }
            // This explicit action can run application constructors and overridden model methods.
            // It never executes a model query or exports connection configuration/row values.
            $model = $reflection->newInstance();
            $connection = $model->getConnection()->getName();
            $table = $model->getTable();
            if (!is_string($connection) || $connection === '' || strlen($connection) > 128 || preg_match('/[\x00-\x1f]/', $connection) || !is_string($table) || $table === '' || strlen($table) > 512 || preg_match('/[\x00-\x1f]/', $table)) { throw new RuntimeException('Invalid model connection facts.'); }
            $dependencies = []; $pending = [$reflection]; $seen = [];
            while ($pending !== []) {
                $type = array_pop($pending); $typeName = $type->getName();
                if (isset($seen[$typeName])) { continue; } $seen[$typeName] = true;
                if (count($seen) > 64) { throw new RuntimeException('Model declaration graph exceeds its bound.'); }
                $source = $type->getFileName();
                if (!is_string($source) || ($source = realpath($source)) === false || !str_starts_with($source, $root.'/')) { throw new RuntimeException('Model declaration graph is not owned by this project.'); }
                $dependencyFile = substr($source, strlen($root) + 1);
                $dependencyHash = hash_file('fnv1a64', $source);
                if (!is_string($dependencyHash)) { throw new RuntimeException('Model dependency fingerprint unavailable.'); }
                $dependencies[$dependencyFile] = ['file' => $dependencyFile, 'hash' => $dependencyHash];
                if (($parent = $type->getParentClass()) !== false) { $pending[] = $parent; }
                foreach ($type->getTraits() as $trait) { $pending[] = $trait; }
            }
            $row['dependencies'] = array_values($dependencies);
            $row['available'] = true; $row['connection'] = $connection; $row['table'] = $table;
        } catch (\Throwable $_) {
            $row['reason'] = 'Framework could not resolve this exact owned model.';
        }
        $result[] = $row;
    }
    echo json_encode(['defaultConnection' => $default, 'models' => $result], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}
if ($mode === 'schema') {
    $selected = $options['connection'] ?? null;
    $connections = $app['config']->get('database.connections', []);
    if ($selected !== null && (!is_string($selected) || !is_array($connections) || !array_key_exists($selected, $connections))) { throw new RuntimeException('Selected database connection is not configured.'); }
    $connection = $app['db']->connection($selected);
    $schema = $connection->getSchemaBuilder();
    if (!method_exists($schema, 'getTables') || !method_exists($schema, 'getColumns') || !method_exists($schema, 'getIndexes')) {
        throw new RuntimeException('This framework schema builder does not expose table, column and index metadata.');
    }
    $tables = $schema->getTables();
    if (!is_array($tables) || count($tables) > 128) { throw new RuntimeException('Schema capture exceeds 128 tables.'); }
    $result = [];
    foreach ($tables as $table) {
        $name = $table['name'] ?? null;
        $namespace = $table['schema'] ?? null;
        if (!is_string($name) || strlen($name) > 512 || preg_match('/[\x00-\x1f]/', $name)) { throw new RuntimeException('Invalid schema table name.'); }
        $qualified = is_string($namespace) && $namespace !== '' ? $namespace.'.'.$name : $name;
        $columns = $schema->getColumns($qualified);
        $indexes = $schema->getIndexes($qualified);
        if (!is_array($columns) || count($columns) > 256 || !is_array($indexes) || count($indexes) > 64) { throw new RuntimeException('Schema table exceeds column/index bounds.'); }
        $safeColumns = [];
        foreach ($columns as $column) {
            $safeColumn = ['name' => (string) ($column['name'] ?? ''), 'type' => (string) ($column['type_name'] ?? $column['type'] ?? 'unknown'), 'nullable' => (bool) ($column['nullable'] ?? false)];
            // These are schema descriptors only. Never copy defaults, comments, generated
            // expressions, connection settings or any row values into the snapshot.
            if (isset($column['type']) && is_string($column['type'])) { $safeColumn['fullType'] = $column['type']; }
            foreach (['auto_increment' => 'autoIncrement', 'unsigned' => 'unsigned'] as $key => $output) {
                if (array_key_exists($key, $column) && is_bool($column[$key])) { $safeColumn[$output] = $column[$key]; }
            }
            foreach (['length', 'precision', 'scale'] as $key) {
                if (array_key_exists($key, $column) && is_int($column[$key])) { $safeColumn[$key] = $column[$key]; }
            }
            if (isset($column['values']) && is_array($column['values'])) { $safeColumn['values'] = array_values($column['values']); }
            $safeColumns[] = $safeColumn;
        }
        $safeIndexes = [];
        foreach ($indexes as $index) {
            $safeIndexes[] = ['name' => (string) ($index['name'] ?? ''), 'columns' => array_values($index['columns'] ?? []), 'unique' => (bool) ($index['unique'] ?? false), 'primary' => (bool) ($index['primary'] ?? false)];
        }
        $result[] = ['name' => $name, 'schema' => is_string($namespace) ? $namespace : '', 'columns' => $safeColumns, 'indexes' => $safeIndexes];
    }
    echo json_encode(['connection' => $selected ?? (string) $app['config']->get('database.default', 'default'), 'tables' => $result], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}
if ($mode === 'database-sources') {
    $connections = $app['config']->get('database.connections', []);
    if (!is_array($connections) || count($connections) > 32) { throw new RuntimeException('Database inventory exceeds 32 declared connections.'); }
    $result = [];
    foreach ($connections as $name => $_) {
        if (!is_string($name) || $name === '' || strlen($name) > 128 || preg_match('/[\\x00-\\x1f]/', $name)) { throw new RuntimeException('Invalid database connection name.'); }
        $safe = ['name' => $name, 'available' => true];
        try {
            // Resolves URL config without connecting, but never exports URL/password/options.
            $config = $app['db']->connection($name)->getConfig();
            if (!empty($config['unix_socket']) || !empty($config['read']) || !empty($config['write']) || !empty($config['options']) || (isset($config['sslmode']) && $config['sslmode'] !== 'prefer')) {
                $result[] = ['name' => $name, 'available' => false, 'reason' => 'Connection transport options require manual configuration.'];
                continue;
            }
            foreach (['driver', 'host', 'port', 'database', 'username'] as $key) {
                $value = $config[$key] ?? null;
                if (is_string($value) || is_int($value) || $value === null) { $safe[$key] = $value; }
                else { $safe = ['name' => $name, 'available' => false, 'reason' => 'Structured connection settings require manual configuration.']; break; }
            }
            if (($safe['driver'] ?? '') === 'sqlite' && is_string($safe['database'] ?? null)) {
                $resolved = realpath($safe['database']);
                if ($resolved !== false) { $safe['database'] = $resolved; }
            }
        } catch (\Throwable $_) {
            // Driver exceptions can include DSNs: return a fixed diagnostic, never their text.
            $safe = ['name' => $name, 'available' => false, 'reason' => 'Framework could not resolve this connection.'];
        }
        $result[] = $safe;
    }
    echo json_encode(['default' => (string) $app['config']->get('database.default', ''), 'connections' => $result], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
    exit;
}
if ($mode === 'database-compare' || $mode === 'database-rows') {
    // Read-only: schema introspection, pending-migration listing and bounded SELECTs. No writes, no connection settings.
    try {
        $selected = $options['connection'] ?? null;
        $declared = $app['config']->get('database.connections', []);
        if ($selected !== null && (!is_string($selected) || !is_array($declared) || !array_key_exists($selected, $declared))) { throw new OpenIdeaLaravelProbeMessage('Selected database connection is not configured.'); }
        $connection = $app['db']->connection($selected);
        $schema = $connection->getSchemaBuilder();
        if (!method_exists($schema, 'getTables') || !method_exists($schema, 'getColumns') || !method_exists($schema, 'getIndexes')) { throw new OpenIdeaLaravelProbeMessage('This framework schema builder does not expose table, column and index metadata.'); }
        $listed = [];
        foreach ($schema->getTables() as $found) {
            $found = (array) $found;
            $name = $found['name'] ?? null;
            $namespace = $found['schema'] ?? null;
            if (!is_string($name) || strlen($name) > 512 || preg_match('/[\x00-\x1f]/', $name)) { continue; }
            $listed[$name] = is_string($namespace) && $namespace !== '' ? $namespace.'.'.$name : $name;
        }
        if ($mode === 'database-compare') {
            $truncated = count($listed) > 256;
            $tables = [];
            foreach (array_slice($listed, 0, 256, true) as $name => $qualified) {
                $columns = array_map(static fn ($column) => (string) ((array) $column)['name'], array_slice($schema->getColumns($qualified), 0, 512));
                $indexes = [];
                foreach (array_slice($schema->getIndexes($qualified), 0, 64) as $index) {
                    $index = (array) $index;
                    $indexes[] = ['columns' => array_map('strval', array_values((array) ($index['columns'] ?? []))), 'primary' => (bool) ($index['primary'] ?? false)];
                }
                $tables[] = ['name' => $name, 'columns' => $columns, 'indexes' => $indexes];
            }
            $migrator = $app->make('migrator');
            if (is_string($selected)) { $migrator->setConnection($selected); }
            $migrations = $migrator->getMigrationFiles(array_merge($migrator->paths(), [$app->databasePath('migrations')]));
            $ran = $migrator->repositoryExists() ? $migrator->getRepository()->getRan() : [];
            $pending = array_slice(array_values(array_diff(array_keys($migrations), $ran)), 0, 500);
            echo json_encode(['connection' => $connection->getName(), 'driver' => $connection->getDriverName(), 'tables' => $tables, 'pending' => $pending, 'truncated' => $truncated], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
            exit;
        }
        $table = $options['table'] ?? null;
        if (!is_string($table) || !array_key_exists($table, $listed)) { throw new OpenIdeaLaravelProbeMessage('The table does not exist in this connection.'); }
        $size = max(10, min(100, (int) ($options['size'] ?? 25)));
        $page = max(0, min(100000, (int) ($options['page'] ?? 0)));
        $qualified = $listed[$table];
        $columns = array_slice(array_map(static fn ($column) => (string) ((array) $column)['name'], $schema->getColumns($qualified)), 0, 128);
        $secret = '/^(pass|passwd|passwords?|passphrase|passcode|pwd|secrets?|tokens?|apikey|authorization|cookies?|credentials?|private|signature|otp|cvv|cvc|card|cardholder|creditcard|hash|salt)$/';
        $personal = '/^(emails?|cpf|cnpj|rg|documents?|phones?|telefones?|celular|mobile|birth|birthday|birthdate|nascimento|address(es)?|endereco|cep|zip|zipcode|ssn|passport|ip|iban|dob)$/';
        // Whole key words only: "token_count" is masked by "token", "tokenizer" and "category" are not.
        $masked = static function (string $column) use ($secret, $personal): bool {
            $spaced = preg_replace(['/([a-z0-9])([A-Z])/', '/([a-zA-Z])([0-9])/'], '$1 $2', $column);
            $words = preg_split('/[^a-z0-9]+/', strtolower($spaced ?? $column), -1, PREG_SPLIT_NO_EMPTY) ?: [];
            if ($words === []) { return false; }
            if (end($words) === 'key') { return true; }
            $candidates = $words;
            for ($i = 1; $i < count($words); $i++) { $candidates[] = $words[$i - 1].$words[$i]; }
            foreach ($candidates as $word) { if (preg_match($secret, $word) || preg_match($personal, $word)) { return true; } }
            return false;
        };
        $query = $connection->table($qualified);
        $total = (clone $query)->count();
        foreach ($schema->getIndexes($qualified) as $index) {
            $index = (array) $index;
            if (!empty($index['primary'])) {
                foreach ((array) ($index['columns'] ?? []) as $keyColumn) { if (in_array($keyColumn, $columns, true)) { $query->orderBy($keyColumn); } }
                break;
            }
        }
        $rows = [];
        foreach ($query->offset($page * $size)->limit($size)->get() as $row) {
            $row = (array) $row;
            $cells = [];
            foreach ($columns as $column) {
                $value = $row[$column] ?? null;
                if ($value !== null && $masked($column)) { $cells[] = '•••'; }
                elseif (is_string($value) && !mb_check_encoding($value, 'UTF-8')) { $cells[] = '(binary, '.strlen($value).' bytes)'; }
                elseif (is_string($value) && mb_strlen($value) > 200) { $cells[] = mb_substr($value, 0, 200).'…'; }
                elseif ($value === null || is_scalar($value)) { $cells[] = $value; }
                else { $cells[] = '(unsupported value)'; }
            }
            $rows[] = $cells;
        }
        echo json_encode(['table' => $table, 'columns' => $columns, 'rows' => $rows, 'total' => $total, 'page' => $page, 'size' => $size, 'masked' => array_values(array_filter($columns, $masked))], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
    } catch (Throwable $error) {
        // Driver and framework errors can include DSNs or SQL values: report the type, never the message.
        echo json_encode(['error' => $error instanceof OpenIdeaLaravelProbeMessage ? $error->getMessage() : 'Database inspection failed ('.get_class($error).').']);
    }
    exit;
}
if ($mode === 'queue-status') {
    $connections = $app['config']->get('queue.connections', []);
    if (!is_array($connections) || count($connections) > 32) {
        throw new RuntimeException('Queue connection inventory exceeds 32 entries.');
    }
    $selected = $options['connection'] ?? null;
    if ($selected !== null && !array_key_exists($selected, $connections)) {
        throw new RuntimeException('Selected queue connection is not configured.');
    }
    $rows = [];
    foreach ($connections as $name => $config) {
        if ($selected !== null && $name !== $selected) { continue; }
        $driver = is_array($config) ? ($config['driver'] ?? '') : '';
        $queue = $options['queue'] ?? ($config['queue'] ?? 'default');
        if (!is_string($name) || strlen($name) > 128 || !is_string($driver) || strlen($driver) > 64 || !is_string($queue) || strlen($queue) > 256) {
            throw new RuntimeException('Unsupported queue connection metadata.');
        }
        $row = ['connection' => $name, 'driver' => $driver, 'queue' => $queue,
            'total' => null, 'pending' => null, 'reserved' => null, 'delayed' => null,
            'running' => null, 'status' => 'unsupported',
            'note' => 'This driver does not expose persistent queue statistics.'];
        if (in_array($driver, ['database', 'redis', 'sqs', 'beanstalkd'], true)) {
            try {
                $connection = $app['queue']->connection($name);
                $known = $connection instanceof \Illuminate\Queue\DatabaseQueue
                    || $connection instanceof \Illuminate\Queue\RedisQueue
                    || $connection instanceof \Illuminate\Queue\SqsQueue
                    || $connection instanceof \Illuminate\Queue\BeanstalkdQueue;
                if (!$known) { throw new RuntimeException('Custom queue adapters are not inspected.'); }
                $complete = true;
                foreach (['total' => 'size', 'pending' => 'pendingSize', 'reserved' => 'reservedSize', 'delayed' => 'delayedSize'] as $field => $method) {
                    if (!is_callable([$connection, $method])) { $complete = false; continue; }
                    $count = $connection->$method($queue);
                    if (!is_int($count) || $count < 0) { $complete = false; continue; }
                    $row[$field] = $count;
                }
                $row['status'] = $complete ? 'available' : 'partial';
                $row['note'] = 'Reserved jobs may be running or awaiting reservation expiry; running job count is unavailable.';
            } catch (Throwable $error) {
                // Driver errors often include credentials/DSNs. Report the type, never the message.
                $row['status'] = 'unavailable';
                $row['note'] = 'Queue statistics unavailable ('.get_class($error).').';
            }
        }
        $rows[] = $row;
    }
    echo json_encode(['queues' => $rows], JSON_THROW_ON_ERROR | JSON_INVALID_UTF8_SUBSTITUTE);
} elseif ($mode === 'workers') {
    if (!is_dir('/proc') || !is_readable('/proc/self/cmdline')) {
        echo json_encode(['status' => 'unavailable', 'note' => 'Worker process visibility requires a readable Linux /proc in the selected PHP runner.', 'workers' => []], JSON_THROW_ON_ERROR);
        exit;
    }
    $root = realpath(getcwd());
    $paths = glob('/proc/[0-9]*/cmdline') ?: [];
    $workers = [];
    $skipped = 0;
    $limited = count($paths) > 4096;
    foreach (array_slice($paths, 0, 4096) as $path) {
        $stream = @fopen($path, 'rb');
        if ($stream === false) { $skipped++; continue; }
        $command = fread($stream, 8193);
        fclose($stream);
        if ($command === false || strlen($command) > 8192) { $skipped++; continue; }
        $arguments = explode("\0", $command);
        $kind = null;
        foreach (['queue:work', 'queue:listen', 'horizon', 'horizon:work', 'horizon:supervisor'] as $candidate) {
            if (in_array($candidate, $arguments, true)) { $kind = $candidate; break; }
        }
        if ($kind === null || !preg_match('/^php(?:[0-9]+(?:\.[0-9]+)*)?$/', basename($arguments[0] ?? ''))) { continue; }
        $pid = (int) basename(dirname($path));
        $cwd = @readlink('/proc/'.$pid.'/cwd');
        $artisan = null;
        foreach ($arguments as $argument) {
            if (basename($argument) === 'artisan') { $artisan = $argument; break; }
        }
        if ($artisan === null || !is_string($cwd)) { $skipped++; continue; }
        $entry = str_starts_with($artisan, '/') ? $artisan : $cwd.'/'.$artisan;
        if (realpath($entry) !== $root.'/artisan') { continue; }
        $workers[] = ['pid' => $pid, 'command' => $kind];
        if (count($workers) >= 256) { $limited = true; break; }
    }
    echo json_encode(['status' => $limited || $skipped > 0 ? 'incomplete' : 'available',
        'note' => 'Observed project Artisan worker processes only; process presence does not prove a job is running.'.($limited || $skipped > 0 ? ' Some process metadata was unavailable or the scan limit was reached.' : ''),
        'workers' => $workers], JSON_THROW_ON_ERROR);
} else {
    throw new RuntimeException('Unsupported inspection mode.');
}
