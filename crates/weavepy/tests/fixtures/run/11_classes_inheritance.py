class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        return self.name + " makes a generic sound"


class Dog(Animal):
    def speak(self):
        return self.name + " says woof"


class Puppy(Dog):
    def speak(self):
        return super(Puppy, self).speak() + " (and wags tail)"


print(Animal("rex").speak())
print(Dog("rex").speak())
print(Puppy("rex").speak())

print(isinstance(Puppy("p"), Animal))
print(isinstance(Puppy("p"), Dog))
print(isinstance(Animal("a"), Dog))
