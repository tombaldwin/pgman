plugins {
    java
    id("org.springframework.boot") version "3.3.4"
}

dependencies {
    implementation("org.springframework.boot:spring-boot-starter-data-jpa")
    runtimeOnly("org.postgresql:postgresql")
}
