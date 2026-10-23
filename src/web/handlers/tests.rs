use super::skills::{render_skill_md, skill_scope, valid_skill_name, validate_skill_location};

#[test]
fn skill_name_validation() {
    assert!(valid_skill_name("code-review"));
    assert!(valid_skill_name("a"));
    assert!(valid_skill_name("My_Skill2"));
    assert!(!valid_skill_name(""));
    assert!(!valid_skill_name("-x"));
    assert!(!valid_skill_name("a b"));
    assert!(!valid_skill_name("a/b"));
    assert!(!valid_skill_name("../escape"));
    assert!(!valid_skill_name("中文"));
    assert!(!valid_skill_name(&"x".repeat(65)));
}

#[test]
fn rejects_location_outside_skills_roots() {
    let tmp = std::env::temp_dir().join(format!("hailux-skill-loc-{}", uuid::Uuid::new_v4()));
    let skill_dir = tmp.join(".hailux").join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_file = skill_dir.join("SKILL.md");
    std::fs::write(&skill_file, "---\nname: demo\n---\nbody").unwrap();
    let outside = tmp.join("SKILL.md");
    std::fs::write(&outside, "---\nname: evil\n---\nbody").unwrap();
    let root_level = tmp.join(".hailux").join("skills").join("SKILL.md");
    std::fs::write(&root_level, "---\nname: root\n---\nbody").unwrap();

    // skills 根下的 SKILL.md 合法
    assert!(validate_skill_location(&skill_file.display().to_string(), &tmp).is_some());
    // 项目根下（非 skills 子目录）的同名文件非法
    assert!(validate_skill_location(&outside.display().to_string(), &tmp).is_none());
    // skills 根直属的 SKILL.md 非法（parent == 根目录，删除会抹掉整个 skills 根）
    assert!(validate_skill_location(&root_level.display().to_string(), &tmp).is_none());
    // 不存在的路径非法
    assert!(
        validate_skill_location(
            &tmp.join("nope").join("SKILL.md").display().to_string(),
            &tmp
        )
        .is_none()
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn render_and_discover_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("hailux-skill-rt-{}", uuid::Uuid::new_v4()));
    let skill_dir = tmp.join(".hailux").join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        render_skill_md(
            "demo",
            "a demo with: colon\nsecond line",
            "# Body\n\ncontent",
        ),
    )
    .unwrap();

    let skills = crate::agent::skill::discover_skills(&tmp).unwrap();
    let demo = skills.iter().find(|s| s.name == "demo").unwrap();
    assert_eq!(demo.description, "a demo with: colon second line");
    assert!(demo.content.contains("# Body"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn scope_detection() {
    let tmp = std::env::temp_dir().join(format!("hailux-skill-scope-{}", uuid::Uuid::new_v4()));
    let skill_dir = tmp.join(".hailux").join("skills").join("p1");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let loc = skill_dir.join("SKILL.md");
    std::fs::write(&loc, "---\nname: p1\n---\nbody").unwrap();
    assert_eq!(skill_scope(&loc, &tmp), "project");
    assert_eq!(skill_scope(&loc, &tmp.join("other")), "global");

    std::fs::remove_dir_all(&tmp).ok();
}
