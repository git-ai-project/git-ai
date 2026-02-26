#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use std::fs;

#[test]
fn test_realistic_refactoring_sequence() {
    // Test a realistic code refactoring scenario with multiple human and AI edits
    let repo = TestRepo::new();
    let file_path = repo.path().join("calculator.rs");

    // Initial human-written code
    fs::write(
        &file_path,
        "pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn add(&mut self, n: i32) {
        self.value += n;
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial calculator implementation")
        .unwrap();

    // AI adds subtract and multiply methods
    fs::write(
        &file_path,
        "pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn add(&mut self, n: i32) {
        self.value += n;
    }

    pub fn subtract(&mut self, n: i32) {
        self.value -= n;
    }

    pub fn multiply(&mut self, n: i32) {
        self.value *= n;
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "calculator.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds subtract and multiply")
        .unwrap();

    // Human refactors to add error handling
    fs::write(
        &file_path,
        "pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn add(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_add(n).ok_or(\"Overflow\")?;
        Ok(())
    }

    pub fn subtract(&mut self, n: i32) {
        self.value -= n;
    }

    pub fn multiply(&mut self, n: i32) {
        self.value *= n;
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds overflow check to add")
        .unwrap();

    // AI completes the refactoring for other methods
    fs::write(
        &file_path,
        "pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn add(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_add(n).ok_or(\"Overflow\")?;
        Ok(())
    }

    pub fn subtract(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_sub(n).ok_or(\"Underflow\")?;
        Ok(())
    }

    pub fn multiply(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_mul(n).ok_or(\"Overflow\")?;
        Ok(())
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "calculator.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds error handling to other methods")
        .unwrap();

    // Verify final attribution aligns with git blame
    let mut file = repo.filename("calculator.rs");
    file.assert_lines_and_blame(lines![
        "pub struct Calculator {".human(),
        "    value: i32,".human(),
        "}".human(),
        "".human(), // Line 4: empty line from original
        "impl Calculator {".human(),
        "    pub fn new() -> Self {".human(),
        "        Self { value: 0 }".human(),
        "    }".human(),
        "".human(), // Line 9: empty line from original
        "    pub fn add(&mut self, n: i32) -> Result<(), String> {".human(),
        "        self.value = self.value.checked_add(n).ok_or(\"Overflow\")?;".human(),
        "        Ok(())".human(),
        "    }".human(),
        "".ai(), // Line 14: empty line added when AI added subtract (git attributes to AI)
        "    pub fn subtract(&mut self, n: i32) -> Result<(), String> {".ai(),
        "        self.value = self.value.checked_sub(n).ok_or(\"Underflow\")?;".ai(),
        "        Ok(())".ai(),
        "    }".ai(),
        "".ai(), // Line 19: empty line from AI's additions
        "    pub fn multiply(&mut self, n: i32) -> Result<(), String> {".ai(),
        "        self.value = self.value.checked_mul(n).ok_or(\"Overflow\")?;".ai(),
        "        Ok(())".ai(),
        "    }".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_realistic_api_endpoint_expansion() {
    // Test AI expanding an API with multiple endpoints, with human edits in between
    let repo = TestRepo::new();
    let file_path = repo.path().join("handlers.rs");

    // Human writes initial GET endpoint
    fs::write(
        &file_path,
        "use axum::{Json, extract::Path};

pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    let user = fetch_user_from_db(id).await;
    Json(user)
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Add get_user endpoint").unwrap();

    // AI adds POST endpoint
    fs::write(
        &file_path,
        "use axum::{Json, extract::Path};

pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    let user = fetch_user_from_db(id).await;
    Json(user)
}

pub async fn create_user(Json(payload): Json<CreateUser>) -> Json<User> {
    let user = insert_user_to_db(payload).await;
    Json(user)
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "handlers.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds create_user endpoint")
        .unwrap();

    // Human adds validation to create_user
    fs::write(
        &file_path,
        "use axum::{Json, extract::Path};

pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    let user = fetch_user_from_db(id).await;
    Json(user)
}

pub async fn create_user(Json(payload): Json<CreateUser>) -> Result<Json<User>, String> {
    if payload.username.is_empty() {
        return Err(\"Username cannot be empty\".to_string());
    }
    let user = insert_user_to_db(payload).await;
    Ok(Json(user))
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds validation").unwrap();

    // AI adds UPDATE and DELETE endpoints
    fs::write(
        &file_path,
        "use axum::{Json, extract::Path};

pub async fn get_user(Path(id): Path<u32>) -> Json<User> {
    let user = fetch_user_from_db(id).await;
    Json(user)
}

pub async fn create_user(Json(payload): Json<CreateUser>) -> Result<Json<User>, String> {
    if payload.username.is_empty() {
        return Err(\"Username cannot be empty\".to_string());
    }
    let user = insert_user_to_db(payload).await;
    Ok(Json(user))
}

pub async fn update_user(Path(id): Path<u32>, Json(payload): Json<UpdateUser>) -> Json<User> {
    let user = update_user_in_db(id, payload).await;
    Json(user)
}

pub async fn delete_user(Path(id): Path<u32>) -> Json<()> {
    delete_user_from_db(id).await;
    Json(())
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "handlers.rs"])
        .unwrap();
    repo.stage_all_and_commit("AI adds update and delete endpoints")
        .unwrap();

    // Human refactors error handling across all endpoints
    fs::write(
        &file_path,
        "use axum::{Json, extract::Path, http::StatusCode};

pub async fn get_user(Path(id): Path<u32>) -> Result<Json<User>, StatusCode> {
    let user = fetch_user_from_db(id).await?;
    Ok(Json(user))
}

pub async fn create_user(Json(payload): Json<CreateUser>) -> Result<Json<User>, StatusCode> {
    if payload.username.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let user = insert_user_to_db(payload).await?;
    Ok(Json(user))
}

pub async fn update_user(Path(id): Path<u32>, Json(payload): Json<UpdateUser>) -> Json<User> {
    let user = update_user_in_db(id, payload).await;
    Json(user)
}

pub async fn delete_user(Path(id): Path<u32>) -> Json<()> {
    delete_user_from_db(id).await;
    Json(())
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human refactors error handling")
        .unwrap();

    // Verify attribution aligns with git blame
    let mut file = repo.filename("handlers.rs");
    file.assert_lines_and_blame(lines![
        "use axum::{Json, extract::Path, http::StatusCode};".human(),
        "".human(),
        "pub async fn get_user(Path(id): Path<u32>) -> Result<Json<User>, StatusCode> {".human(),
        "    let user = fetch_user_from_db(id).await?;".human(),
        "    Ok(Json(user))".human(),
        "}".ai(),  // Line 6: git attributes closing brace to AI due to AI adding next function
        "".ai(),
        "pub async fn create_user(Json(payload): Json<CreateUser>) -> Result<Json<User>, StatusCode> {".human(),
        "    if payload.username.is_empty() {".human(),
        "        return Err(StatusCode::BAD_REQUEST);".human(),
        "    }".human(),
        "    let user = insert_user_to_db(payload).await?;".human(),
        "    Ok(Json(user))".human(),
        "}".ai(),  // Line 14: git attributes closing brace to AI
        "".ai(),
        "pub async fn update_user(Path(id): Path<u32>, Json(payload): Json<UpdateUser>) -> Json<User> {".ai(),
        "    let user = update_user_in_db(id, payload).await;".ai(),
        "    Json(user)".ai(),
        "}".ai(),
        "".ai(),
        "pub async fn delete_user(Path(id): Path<u32>) -> Json<()> {".ai(),
        "    delete_user_from_db(id).await;".ai(),
        "    Json(())".ai(),
        "}".human(),  // Line 24: final closing brace stays human from original file
    ]);
}

#[test]
fn test_realistic_test_file_evolution() {
    // Test evolution of a test file with AI adding tests and human refactoring
    let repo = TestRepo::new();
    let file_path = repo.path().join("tests.rs");

    // Human writes initial test
    fs::write(
        &file_path,
        "#[test]
fn test_addition() {
    assert_eq!(2 + 2, 4);
}
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial test").unwrap();

    // AI adds more test cases
    fs::write(
        &file_path,
        "#[test]
fn test_addition() {
    assert_eq!(2 + 2, 4);
}

#[test]
fn test_subtraction() {
    assert_eq!(5 - 3, 2);
}

#[test]
fn test_multiplication() {
    assert_eq!(3 * 4, 12);
}
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "tests.rs"]).unwrap();
    repo.stage_all_and_commit("AI adds more tests").unwrap();

    // Human refactors to use test module
    fs::write(
        &file_path,
        "mod arithmetic_tests {
    #[test]
    fn test_addition() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn test_subtraction() {
        assert_eq!(5 - 3, 2);
    }

    #[test]
    fn test_multiplication() {
        assert_eq!(3 * 4, 12);
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds module wrapper")
        .unwrap();

    // AI adds division test
    fs::write(
        &file_path,
        "mod arithmetic_tests {
    #[test]
    fn test_addition() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn test_subtraction() {
        assert_eq!(5 - 3, 2);
    }

    #[test]
    fn test_multiplication() {
        assert_eq!(3 * 4, 12);
    }

    #[test]
    fn test_division() {
        assert_eq!(12 / 3, 4);
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "tests.rs"]).unwrap();
    repo.stage_all_and_commit("AI adds division test").unwrap();

    // Human adds edge case test
    fs::write(
        &file_path,
        "mod arithmetic_tests {
    #[test]
    fn test_addition() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn test_subtraction() {
        assert_eq!(5 - 3, 2);
    }

    #[test]
    fn test_multiplication() {
        assert_eq!(3 * 4, 12);
    }

    #[test]
    fn test_division() {
        assert_eq!(12 / 3, 4);
    }

    #[test]
    #[should_panic]
    fn test_division_by_zero() {
        let _ = 1 / 0;
    }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds edge case test")
        .unwrap();

    // Verify git alignment
    // Without move detection, when human refactored to add module wrapper,
    // git attributes all indented lines to human, but blank lines stay with AI
    let mut file = repo.filename("tests.rs");
    file.assert_lines_and_blame(lines![
        "mod arithmetic_tests {".human(),
        "    #[test]".human(),
        "    fn test_addition() {".human(),
        "        assert_eq!(2 + 2, 4);".human(),
        "    }".human(), // Line 5: attributed to human who added indentation
        "".ai(),         // Blank lines stay attributed to AI who originally added them
        "    #[test]".human(),
        "    fn test_subtraction() {".human(),
        "        assert_eq!(5 - 3, 2);".human(),
        "    }".human(),
        "".ai(), // Blank line stays with AI
        "    #[test]".human(),
        "    fn test_multiplication() {".human(),
        "        assert_eq!(3 * 4, 12);".human(),
        "    }".human(),
        "".ai(), // Blank line added by AI with division test
        "    #[test]".ai(),
        "    fn test_division() {".ai(),
        "        assert_eq!(12 / 3, 4);".ai(),
        "    }".ai(),
        "".human(), // Blank line added by human with edge case test
        "    #[test]".human(),
        "    #[should_panic]".human(),
        "    fn test_division_by_zero() {".human(),
        "        let _ = 1 / 0;".human(),
        "    }".human(),
        "}".human(), // Line 27: closing brace from human's module wrapper
    ]);
}

#[test]
fn test_realistic_config_file_with_comments() {
    // Test AI and human editing a config file with comments
    let repo = TestRepo::new();
    let file_path = repo.path().join("config.toml");

    // Human creates initial config
    fs::write(
        &file_path,
        "[server]
host = \"localhost\"
port = 8080
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial config").unwrap();

    // AI adds database config
    fs::write(
        &file_path,
        "[server]
host = \"localhost\"
port = 8080

[database]
url = \"postgresql://localhost/mydb\"
max_connections = 10
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "config.toml"])
        .unwrap();
    repo.stage_all_and_commit("AI adds database config")
        .unwrap();

    // Human adds comments and changes port
    fs::write(
        &file_path,
        "# Server configuration
[server]
host = \"localhost\"
# Changed to use port 3000
port = 3000

[database]
url = \"postgresql://localhost/mydb\"
max_connections = 10
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds comments and changes port")
        .unwrap();

    // AI adds logging config
    fs::write(
        &file_path,
        "# Server configuration
[server]
host = \"localhost\"
# Changed to use port 3000
port = 3000

[database]
url = \"postgresql://localhost/mydb\"
max_connections = 10

[logging]
level = \"info\"
format = \"json\"
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "config.toml"])
        .unwrap();
    repo.stage_all_and_commit("AI adds logging config").unwrap();

    // Verify alignment with git
    let mut file = repo.filename("config.toml");
    file.assert_lines_and_blame(lines![
        "# Server configuration".human(),
        "[server]".human(),
        "host = \"localhost\"".human(),
        "# Changed to use port 3000".human(),
        "port = 3000".human(),
        "".ai(), // Line 6: git attributes empty line to AI (inserted between sections)
        "[database]".ai(),
        "url = \"postgresql://localhost/mydb\"".ai(),
        "max_connections = 10".ai(),
        "".ai(),
        "[logging]".ai(),
        "level = \"info\"".ai(),
        "format = \"json\"".ai(),
    ]);
}

#[test]
fn test_realistic_jsx_component_development() {
    // Test AI and human building a React component together
    let repo = TestRepo::new();
    let file_path = repo.path().join("Button.jsx");

    // Human creates basic component
    fs::write(
        &file_path,
        "export function Button({ children }) {
  return <button>{children}</button>;
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial Button component")
        .unwrap();

    // AI adds onClick and styling props
    fs::write(
        &file_path,
        "export function Button({ children, onClick, className }) {
  return (
    <button onClick={onClick} className={className}>
      {children}
    </button>
  );
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "Button.jsx"])
        .unwrap();
    repo.stage_all_and_commit("AI adds onClick and className props")
        .unwrap();

    // Human adds variant prop with styles
    fs::write(
        &file_path,
        "export function Button({ children, onClick, className, variant = 'primary' }) {
  const baseStyles = 'px-4 py-2 rounded';
  const variantStyles = variant === 'primary' ? 'bg-blue-500 text-white' : 'bg-gray-200';

  return (
    <button onClick={onClick} className={`${baseStyles} ${variantStyles} ${className}`}>
      {children}
    </button>
  );
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds variant styling")
        .unwrap();

    // AI adds disabled state
    fs::write(
        &file_path,
        "export function Button({ children, onClick, className, variant = 'primary', disabled = false }) {
  const baseStyles = 'px-4 py-2 rounded';
  const variantStyles = variant === 'primary' ? 'bg-blue-500 text-white' : 'bg-gray-200';
  const disabledStyles = disabled ? 'opacity-50 cursor-not-allowed' : '';

  return (
    <button
      onClick={disabled ? undefined : onClick}
      className={`${baseStyles} ${variantStyles} ${disabledStyles} ${className}`}
      disabled={disabled}
    >
      {children}
    </button>
  );
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "Button.jsx"])
        .unwrap();
    repo.stage_all_and_commit("AI adds disabled state").unwrap();

    // Verify git blame alignment
    let mut file = repo.filename("Button.jsx");
    file.assert_lines_and_blame(lines![
        "export function Button({ children, onClick, className, variant = 'primary', disabled = false }) {".ai(),
        "  const baseStyles = 'px-4 py-2 rounded';".human(),
        "  const variantStyles = variant === 'primary' ? 'bg-blue-500 text-white' : 'bg-gray-200';".human(),
        "  const disabledStyles = disabled ? 'opacity-50 cursor-not-allowed' : '';".ai(),
        "  ".human(),  // Line 5: git attributes whitespace-only line to human
        "  return (".ai(),
        "    <button".human(),  // Line 7: git attributes to Test User (from human edits)
        "      onClick={disabled ? undefined : onClick}".ai(),
        "      className={`${baseStyles} ${variantStyles} ${disabledStyles} ${className}`}".ai(),
        "      disabled={disabled}".ai(),
        "    >".human(),  // Line 11: git attributes to Test User
        "      {children}".human(),
        "    </button>".human(),
        "  );".ai(),
        "}".human(),  // Line 15: final closing brace stays human
    ]);
}

#[test]
fn test_realistic_class_with_multiple_methods() {
    // Test complex class evolution with multiple method additions and modifications
    let repo = TestRepo::new();
    let file_path = repo.path().join("UserManager.ts");

    // Human creates initial class with one method
    fs::write(
        &file_path,
        "export class UserManager {
  private users: Map<string, User> = new Map();

  constructor() {}

  addUser(user: User): void {
    this.users.set(user.id, user);
  }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial UserManager class")
        .unwrap();

    // AI adds getUser and removeUser
    fs::write(
        &file_path,
        "export class UserManager {
  private users: Map<string, User> = new Map();

  constructor() {}

  addUser(user: User): void {
    this.users.set(user.id, user);
  }

  getUser(id: string): User | undefined {
    return this.users.get(id);
  }

  removeUser(id: string): boolean {
    return this.users.delete(id);
  }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "UserManager.ts"])
        .unwrap();
    repo.stage_all_and_commit("AI adds getUser and removeUser")
        .unwrap();

    // Human refactors addUser to validate
    fs::write(
        &file_path,
        "export class UserManager {
  private users: Map<string, User> = new Map();

  constructor() {}

  addUser(user: User): void {
    if (!user.id || !user.email) {
      throw new Error('Invalid user');
    }
    this.users.set(user.id, user);
  }

  getUser(id: string): User | undefined {
    return this.users.get(id);
  }

  removeUser(id: string): boolean {
    return this.users.delete(id);
  }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds validation to addUser")
        .unwrap();

    // AI adds updateUser method
    fs::write(
        &file_path,
        "export class UserManager {
  private users: Map<string, User> = new Map();

  constructor() {}

  addUser(user: User): void {
    if (!user.id || !user.email) {
      throw new Error('Invalid user');
    }
    this.users.set(user.id, user);
  }

  getUser(id: string): User | undefined {
    return this.users.get(id);
  }

  updateUser(id: string, updates: Partial<User>): User | undefined {
    const user = this.users.get(id);
    if (!user) return undefined;
    const updatedUser = { ...user, ...updates };
    this.users.set(id, updatedUser);
    return updatedUser;
  }

  removeUser(id: string): boolean {
    return this.users.delete(id);
  }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "UserManager.ts"])
        .unwrap();
    repo.stage_all_and_commit("AI adds updateUser method")
        .unwrap();

    // Human adds getAllUsers and count
    fs::write(
        &file_path,
        "export class UserManager {
  private users: Map<string, User> = new Map();

  constructor() {}

  addUser(user: User): void {
    if (!user.id || !user.email) {
      throw new Error('Invalid user');
    }
    this.users.set(user.id, user);
  }

  getUser(id: string): User | undefined {
    return this.users.get(id);
  }

  getAllUsers(): User[] {
    return Array.from(this.users.values());
  }

  getUserCount(): number {
    return this.users.size;
  }

  updateUser(id: string, updates: Partial<User>): User | undefined {
    const user = this.users.get(id);
    if (!user) return undefined;
    const updatedUser = { ...user, ...updates };
    this.users.set(id, updatedUser);
    return updatedUser;
  }

  removeUser(id: string): boolean {
    return this.users.delete(id);
  }
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds getAllUsers and getUserCount")
        .unwrap();

    // Verify git blame alignment
    let mut file = repo.filename("UserManager.ts");
    file.assert_lines_and_blame(lines![
        "export class UserManager {".human(),
        "  private users: Map<string, User> = new Map();".human(),
        "".human(),
        "  constructor() {}".human(),
        "".human(),
        "  addUser(user: User): void {".human(),
        "    if (!user.id || !user.email) {".human(),
        "      throw new Error('Invalid user');".human(),
        "    }".human(),
        "    this.users.set(user.id, user);".human(),
        "  }".human(),
        "".ai(), // Line 12: git attributes empty line to AI
        "  getUser(id: string): User | undefined {".ai(),
        "    return this.users.get(id);".ai(),
        "  }".ai(),
        "".ai(),
        "  getAllUsers(): User[] {".human(),
        "    return Array.from(this.users.values());".human(),
        "  }".human(),
        "".human(),
        "  getUserCount(): number {".human(),
        "    return this.users.size;".human(),
        "  }".human(),
        "".human(), // Line 24: human empty line
        "  updateUser(id: string, updates: Partial<User>): User | undefined {".ai(),
        "    const user = this.users.get(id);".ai(),
        "    if (!user) return undefined;".ai(),
        "    const updatedUser = { ...user, ...updates };".ai(),
        "    this.users.set(id, updatedUser);".ai(),
        "    return updatedUser;".ai(),
        "  }".ai(),
        "".ai(),
        "  removeUser(id: string): boolean {".ai(),
        "    return this.users.delete(id);".ai(),
        "  }".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_realistic_middleware_chain_development() {
    // Test building middleware with AI and human working together
    let repo = TestRepo::new();
    let file_path = repo.path().join("middleware.ts");

    // Human creates basic logging middleware
    fs::write(
        &file_path,
        "export function loggerMiddleware(req, res, next) {
  console.log(`${req.method} ${req.path}`);
  next();
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial logger middleware")
        .unwrap();

    // AI adds auth middleware
    fs::write(
        &file_path,
        "export function loggerMiddleware(req, res, next) {
  console.log(`${req.method} ${req.path}`);
  next();
}

export function authMiddleware(req, res, next) {
  const token = req.headers['authorization'];
  if (!token) {
    return res.status(401).json({ error: 'Unauthorized' });
  }
  next();
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "middleware.ts"])
        .unwrap();
    repo.stage_all_and_commit("AI adds auth middleware")
        .unwrap();

    // Human improves logging with timestamps
    fs::write(
        &file_path,
        "export function loggerMiddleware(req, res, next) {
  const timestamp = new Date().toISOString();
  console.log(`[${timestamp}] ${req.method} ${req.path}`);
  next();
}

export function authMiddleware(req, res, next) {
  const token = req.headers['authorization'];
  if (!token) {
    return res.status(401).json({ error: 'Unauthorized' });
  }
  next();
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds timestamps to logger")
        .unwrap();

    // AI adds rate limiting middleware
    fs::write(
        &file_path,
        "export function loggerMiddleware(req, res, next) {
  const timestamp = new Date().toISOString();
  console.log(`[${timestamp}] ${req.method} ${req.path}`);
  next();
}

export function authMiddleware(req, res, next) {
  const token = req.headers['authorization'];
  if (!token) {
    return res.status(401).json({ error: 'Unauthorized' });
  }
  next();
}

const rateLimitStore = new Map();

export function rateLimitMiddleware(limit = 100) {
  return (req, res, next) => {
    const ip = req.ip;
    const count = rateLimitStore.get(ip) || 0;
    if (count >= limit) {
      return res.status(429).json({ error: 'Rate limit exceeded' });
    }
    rateLimitStore.set(ip, count + 1);
    next();
  };
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "middleware.ts"])
        .unwrap();
    repo.stage_all_and_commit("AI adds rate limiting").unwrap();

    // Human adds error handling middleware
    fs::write(
        &file_path,
        "export function loggerMiddleware(req, res, next) {
  const timestamp = new Date().toISOString();
  console.log(`[${timestamp}] ${req.method} ${req.path}`);
  next();
}

export function authMiddleware(req, res, next) {
  const token = req.headers['authorization'];
  if (!token) {
    return res.status(401).json({ error: 'Unauthorized' });
  }
  next();
}

const rateLimitStore = new Map();

export function rateLimitMiddleware(limit = 100) {
  return (req, res, next) => {
    const ip = req.ip;
    const count = rateLimitStore.get(ip) || 0;
    if (count >= limit) {
      return res.status(429).json({ error: 'Rate limit exceeded' });
    }
    rateLimitStore.set(ip, count + 1);
    next();
  };
}

export function errorHandlerMiddleware(err, req, res, next) {
  console.error('Error:', err);
  res.status(500).json({ error: 'Internal server error' });
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds error handler")
        .unwrap();

    // Verify git alignment
    let mut file = repo.filename("middleware.ts");
    file.assert_lines_and_blame(lines![
        "export function loggerMiddleware(req, res, next) {".human(),
        "  const timestamp = new Date().toISOString();".human(),
        "  console.log(`[${timestamp}] ${req.method} ${req.path}`);".human(),
        "  next();".human(),
        "}".ai(), // Line 5: git attributes closing brace to AI
        "".ai(),
        "export function authMiddleware(req, res, next) {".ai(),
        "  const token = req.headers['authorization'];".ai(),
        "  if (!token) {".ai(),
        "    return res.status(401).json({ error: 'Unauthorized' });".ai(),
        "  }".ai(),
        "  next();".ai(),
        "}".ai(), // Line 13: git attributes closing brace to AI
        "".ai(),
        "const rateLimitStore = new Map();".ai(),
        "".ai(),
        "export function rateLimitMiddleware(limit = 100) {".ai(),
        "  return (req, res, next) => {".ai(),
        "    const ip = req.ip;".ai(),
        "    const count = rateLimitStore.get(ip) || 0;".ai(),
        "    if (count >= limit) {".ai(),
        "      return res.status(429).json({ error: 'Rate limit exceeded' });".ai(),
        "    }".ai(),
        "    rateLimitStore.set(ip, count + 1);".ai(),
        "    next();".ai(),
        "  };".ai(),
        "}".human(), // Line 27: git attributes to Test User (human adds error handler after this)
        "".human(),
        "export function errorHandlerMiddleware(err, req, res, next) {".human(),
        "  console.error('Error:', err);".human(),
        "  res.status(500).json({ error: 'Internal server error' });".human(),
        "}".human(), // Line 32: final closing brace stays human
    ]);
}

#[test]
fn test_realistic_sql_migration_sequence() {
    // Test AI and human collaborating on database migrations
    let repo = TestRepo::new();
    let file_path = repo.path().join("001_initial.sql");

    // Human creates initial users table
    fs::write(
        &file_path,
        "-- Initial migration
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  email VARCHAR(255) NOT NULL
);
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial migration").unwrap();

    // AI adds indexes and constraints
    fs::write(
        &file_path,
        "-- Initial migration
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  email VARCHAR(255) NOT NULL,
  UNIQUE(email)
);

CREATE INDEX idx_users_email ON users(email);
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "001_initial.sql"])
        .unwrap();
    repo.stage_all_and_commit("AI adds indexes and constraints")
        .unwrap();

    // Human adds created_at column
    fs::write(
        &file_path,
        "-- Initial migration
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  email VARCHAR(255) NOT NULL,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(email)
);

CREATE INDEX idx_users_email ON users(email);
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds created_at").unwrap();

    // AI adds posts table with foreign key
    fs::write(
        &file_path,
        "-- Initial migration
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  email VARCHAR(255) NOT NULL,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(email)
);

CREATE INDEX idx_users_email ON users(email);

CREATE TABLE posts (
  id SERIAL PRIMARY KEY,
  user_id INTEGER NOT NULL,
  title VARCHAR(255) NOT NULL,
  content TEXT,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_posts_user_id ON posts(user_id);
",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "001_initial.sql"])
        .unwrap();
    repo.stage_all_and_commit("AI adds posts table").unwrap();

    // Verify alignment
    let mut file = repo.filename("001_initial.sql");
    file.assert_lines_and_blame(lines![
        "-- Initial migration".human(),
        "CREATE TABLE users (".human(),
        "  id SERIAL PRIMARY KEY,".human(),
        "  email VARCHAR(255) NOT NULL,".ai(), // Line 4: git attributes to AI due to adding UNIQUE constraint
        "  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,".human(),
        "  UNIQUE(email)".ai(),
        // @todo this is caused by last line diff bug.
        // started showing up when we toggled off move feature flag
        ");".human(),
        "".ai(),
        "CREATE INDEX idx_users_email ON users(email);".ai(),
        "".ai(),
        "CREATE TABLE posts (".ai(),
        "  id SERIAL PRIMARY KEY,".ai(),
        "  user_id INTEGER NOT NULL,".ai(),
        "  title VARCHAR(255) NOT NULL,".ai(),
        "  content TEXT,".ai(),
        "  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,".ai(),
        "  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE".ai(),
        ");".ai(),
        "".ai(),
        "CREATE INDEX idx_posts_user_id ON posts(user_id);".ai(),
    ]);
}

#[test]
fn test_realistic_refactoring_with_deletions() {
    // Test removing deprecated code - AI removes old API, human cleans up more
    let repo = TestRepo::new();
    let file_path = repo.path().join("api.rs");

    // Human creates initial API with old and new versions
    fs::write(
        &file_path,
        "// Legacy API - deprecated
pub fn process_data_v1(data: &str) -> String {
    data.to_uppercase()
}

pub fn process_data_v1_with_trim(data: &str) -> String {
    data.trim().to_uppercase()
}

// New API
pub fn process_data(data: &str) -> Result<String, String> {
    if data.is_empty() {
        return Err(\"Empty data\".to_string());
    }
    Ok(data.trim().to_uppercase())
}

// Helper function
pub fn validate_input(data: &str) -> bool {
    !data.is_empty()
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial API with legacy functions")
        .unwrap();

    // AI removes deprecated v1 functions
    fs::write(
        &file_path,
        "// New API
pub fn process_data(data: &str) -> Result<String, String> {
    if data.is_empty() {
        return Err(\"Empty data\".to_string());
    }
    Ok(data.trim().to_uppercase())
}

// Helper function
pub fn validate_input(data: &str) -> bool {
    !data.is_empty()
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "api.rs"]).unwrap();
    repo.stage_all_and_commit("AI removes deprecated v1 functions")
        .unwrap();

    // Human adds new function and improves validation
    fs::write(
        &file_path,
        "// New API
pub fn process_data(data: &str) -> Result<String, String> {
    if data.is_empty() {
        return Err(\"Empty data\".to_string());
    }
    Ok(data.trim().to_uppercase())
}

pub fn process_batch(items: &[&str]) -> Vec<Result<String, String>> {
    items.iter().map(|item| process_data(item)).collect()
}

// Helper function
pub fn validate_input(data: &str) -> bool {
    !data.trim().is_empty()
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds batch processing and improves validation")
        .unwrap();

    // AI removes comment and adds error type
    fs::write(
        &file_path,
        "pub type ProcessError = String;

pub fn process_data(data: &str) -> Result<String, ProcessError> {
    if data.is_empty() {
        return Err(\"Empty data\".to_string());
    }
    Ok(data.trim().to_uppercase())
}

pub fn process_batch(items: &[&str]) -> Vec<Result<String, ProcessError>> {
    items.iter().map(|item| process_data(item)).collect()
}

pub fn validate_input(data: &str) -> bool {
    !data.trim().is_empty()
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "api.rs"]).unwrap();
    repo.stage_all_and_commit("AI adds error type alias")
        .unwrap();

    // Verify attribution after deletions
    let mut file = repo.filename("api.rs");
    file.assert_lines_and_blame(lines![
        "pub type ProcessError = String;".ai(),
        "".ai(),
        "pub fn process_data(data: &str) -> Result<String, ProcessError> {".ai(),
        "    if data.is_empty() {".human(),
        "        return Err(\"Empty data\".to_string());".human(),
        "    }".human(),
        "    Ok(data.trim().to_uppercase())".human(),
        "}".human(),
        "".human(),
        "pub fn process_batch(items: &[&str]) -> Vec<Result<String, ProcessError>> {".ai(),
        "    items.iter().map(|item| process_data(item)).collect()".human(),
        "}".human(),
        "".human(),
        "pub fn validate_input(data: &str) -> bool {".human(),
        "    !data.trim().is_empty()".human(),
        "}".human(),
    ]);
}

#[test]
fn test_realistic_formatting_and_whitespace_changes() {
    // Test code formatting changes - human writes compact, AI reformats, human adds features
    let repo = TestRepo::new();
    let file_path = repo.path().join("config.py");

    // Human writes compact Python config
    fs::write(
        &file_path,
        "class Config:
    def __init__(self):
        self.debug = False
        self.port = 8000
        self.host = \"localhost\"

    def get_url(self):
        return f\"http://{self.host}:{self.port}\"",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial compact config").unwrap();

    // AI reformats with better spacing and adds docstrings
    fs::write(
        &file_path,
        "class Config:
    \"\"\"Application configuration.\"\"\"

    def __init__(self):
        \"\"\"Initialize with default settings.\"\"\"
        self.debug = False
        self.port = 8000
        self.host = \"localhost\"

    def get_url(self):
        \"\"\"Get the full application URL.\"\"\"
        return f\"http://{self.host}:{self.port}\"",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "config.py"])
        .unwrap();
    repo.stage_all_and_commit("AI adds docstrings and formatting")
        .unwrap();

    // Human adds database config
    fs::write(
        &file_path,
        "class Config:
    \"\"\"Application configuration.\"\"\"

    def __init__(self):
        \"\"\"Initialize with default settings.\"\"\"
        self.debug = False
        self.port = 8000
        self.host = \"localhost\"
        self.db_url = \"sqlite:///app.db\"

    def get_url(self):
        \"\"\"Get the full application URL.\"\"\"
        return f\"http://{self.host}:{self.port}\"

    def get_database_url(self):
        return self.db_url",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds database config")
        .unwrap();

    // AI reformats new method with docstring
    fs::write(
        &file_path,
        "class Config:
    \"\"\"Application configuration.\"\"\"

    def __init__(self):
        \"\"\"Initialize with default settings.\"\"\"
        self.debug = False
        self.port = 8000
        self.host = \"localhost\"
        self.db_url = \"sqlite:///app.db\"

    def get_url(self):
        \"\"\"Get the full application URL.\"\"\"
        return f\"http://{self.host}:{self.port}\"

    def get_database_url(self):
        \"\"\"Get the database connection URL.\"\"\"
        return self.db_url",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "config.py"])
        .unwrap();
    repo.stage_all_and_commit("AI adds docstring to new method")
        .unwrap();

    // Verify attribution with whitespace changes
    let mut file = repo.filename("config.py");
    file.assert_lines_and_blame(lines![
        "class Config:".human(),
        "    \"\"\"Application configuration.\"\"\"".ai(),
        "    ".ai(),
        "    def __init__(self):".human(),
        "        \"\"\"Initialize with default settings.\"\"\"".ai(),
        "        self.debug = False".human(),
        "        self.port = 8000".human(),
        "        self.host = \"localhost\"".human(),
        "        self.db_url = \"sqlite:///app.db\"".human(),
        "    ".human(), // Line 10: git attributes whitespace to human
        "    def get_url(self):".human(),
        "        \"\"\"Get the full application URL.\"\"\"".ai(),
        "        return f\"http://{self.host}:{self.port}\"".human(),
        "    ".human(), // Line 14: git attributes to human
        "    def get_database_url(self):".human(),
        "        \"\"\"Get the database connection URL.\"\"\"".ai(),
        "        return self.db_url".human(),
    ]);
}

#[test]
fn test_realistic_multi_file_commit() {
    // Test editing multiple related files in a single commit
    let repo = TestRepo::new();
    let model_path = repo.path().join("models.rs");
    let handler_path = repo.path().join("handlers.rs");
    let schema_path = repo.path().join("schema.sql");

    // Human creates initial model
    fs::write(
        &model_path,
        "pub struct User {
    pub id: i32,
    pub name: String,
}",
    )
    .unwrap();

    fs::write(
        &handler_path,
        "use crate::models::User;

pub fn get_user(id: i32) -> Option<User> {
    None
}",
    )
    .unwrap();

    fs::write(
        &schema_path,
        "CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Initial user model and schema")
        .unwrap();

    // AI adds email field to all three files
    fs::write(
        &model_path,
        "pub struct User {
    pub id: i32,
    pub name: String,
    pub email: String,
}",
    )
    .unwrap();

    fs::write(
        &handler_path,
        "use crate::models::User;

pub fn get_user(id: i32) -> Option<User> {
    None
}

pub fn get_user_by_email(email: &str) -> Option<User> {
    None
}",
    )
    .unwrap();

    fs::write(
        &schema_path,
        "CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE NOT NULL
);

CREATE INDEX idx_users_email ON users(email);",
    )
    .unwrap();

    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();
    repo.stage_all_and_commit("AI adds email field across all files")
        .unwrap();

    // Human adds validation
    fs::write(
        &model_path,
        "pub struct User {
    pub id: i32,
    pub name: String,
    pub email: String,
}

impl User {
    pub fn validate_email(&self) -> bool {
        self.email.contains('@')
    }
}",
    )
    .unwrap();

    fs::write(
        &handler_path,
        "use crate::models::User;

pub fn get_user(id: i32) -> Option<User> {
    None
}

pub fn get_user_by_email(email: &str) -> Option<User> {
    if !email.contains('@') {
        return None;
    }
    None
}",
    )
    .unwrap();

    repo.git_ai(&["checkpoint"]).unwrap();
    repo.stage_all_and_commit("Human adds email validation")
        .unwrap();

    // Verify models.rs
    let mut models_file = repo.filename("models.rs");
    models_file.assert_lines_and_blame(lines![
        "pub struct User {".human(),
        "    pub id: i32,".human(),
        "    pub name: String,".human(),
        "    pub email: String,".ai(),
        "}".human(), // Line 5: git attributes closing brace to human (impl added after)
        "".human(),
        "impl User {".human(),
        "    pub fn validate_email(&self) -> bool {".human(),
        "        self.email.contains('@')".human(),
        "    }".human(),
        "}".human(), // Line 11: stays human
    ]);

    // Verify handlers.rs
    let mut handlers_file = repo.filename("handlers.rs");
    handlers_file.assert_lines_and_blame(lines![
        "use crate::models::User;".human(),
        "".human(),
        "pub fn get_user(id: i32) -> Option<User> {".human(),
        "    None".human(),
        "}".ai(), // Line 5: git attributes closing brace to AI (next function added by AI)
        "".ai(),
        "pub fn get_user_by_email(email: &str) -> Option<User> {".ai(),
        "    if !email.contains('@') {".human(),
        "        return None;".human(),
        "    }".human(),
        "    None".ai(),
        "}".human(), // Line 12: final closing brace stays human
    ]);

    // Verify schema.sql
    let mut schema_file = repo.filename("schema.sql");
    schema_file.assert_lines_and_blame(lines![
        "CREATE TABLE users (".human(),
        "    id INTEGER PRIMARY KEY,".human(),
        "    name TEXT NOT NULL,".ai(), // Line 3: git attributes to AI (comma added)
        "    email TEXT UNIQUE NOT NULL".ai(),
        ");".ai(),
        "".ai(),
        "CREATE INDEX idx_users_email ON users(email);".ai(),
    ]);
}
